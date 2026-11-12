//! Cross-harness Parent ↔ Child agent orchestration, reviewer loops, and SQLite IPC.

pub mod launch;
pub mod mcp;

pub use launch::*;
pub use mcp::*;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::ValueEnum;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::macos;
use crate::sessions::{Agent, Store, canonical_terminal_key, shell_quote};
use crate::{Result, invalid};

static TASK_ID_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum TaskRole {
    Reviewer,
    Worker,
    Advisor,
}

impl TaskRole {
    pub const ALL: [Self; 3] = [Self::Reviewer, Self::Worker, Self::Advisor];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reviewer => "reviewer",
            Self::Worker => "worker",
            Self::Advisor => "advisor",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "worker" => Self::Worker,
            "advisor" => Self::Advisor,
            _ => Self::Reviewer,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Reviewer => "Reviewer",
            Self::Worker => "Worker",
            Self::Advisor => "Advisor",
        }
    }

    pub fn short_badge(self) -> &'static str {
        match self {
            Self::Reviewer => "REV",
            Self::Worker => "WRK",
            Self::Advisor => "ADV",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Reviewer => Self::Worker,
            Self::Worker => Self::Advisor,
            Self::Advisor => Self::Reviewer,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Reviewer => Self::Advisor,
            Self::Worker => Self::Reviewer,
            Self::Advisor => Self::Worker,
        }
    }

    pub fn default_goal(self) -> &'static str {
        match self {
            Self::Reviewer => {
                "Review the current git diff, recent changes, and tests in this workspace. Identify bugs, race conditions, or missing edge cases, and coordinate with the parent agent until all issues are resolved and approved."
            }
            Self::Worker => {
                "Execute the delegated implementation task in this workspace, run relevant tests, and report the changes back to the parent agent."
            }
            Self::Advisor => {
                "Analyze the codebase and problem from a second-opinion perspective and reply to the parent agent with concrete architectural recommendations."
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ReplyVerdict {
    #[value(alias = "changes")]
    ChangesRequested,
    #[value(alias = "lgtm")]
    Approved,
    Done,
    Comment,
}

impl ReplyVerdict {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "changes_requested" | "changes" => Self::ChangesRequested,
            "approved" | "lgtm" => Self::Approved,
            "done" => Self::Done,
            _ => Self::Comment,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChangesRequested => "changes_requested",
            Self::Approved => "approved",
            Self::Done => "done",
            Self::Comment => "comment",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Approved | Self::Done)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    WaitingChild,
    WaitingParent,
    ChangesRequested,
    Approved,
    Done,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitingChild => "waiting_child",
            Self::WaitingParent => "waiting_parent",
            Self::ChangesRequested => "changes_requested",
            Self::Approved => "approved",
            Self::Done => "done",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "waiting_parent" => Self::WaitingParent,
            "changes_requested" => Self::ChangesRequested,
            "approved" => Self::Approved,
            "done" => Self::Done,
            _ => Self::WaitingChild,
        }
    }

    pub fn badge(self) -> &'static str {
        match self {
            Self::WaitingChild => "⇄ WORKING",
            Self::WaitingParent => "⇄ WAITING",
            Self::ChangesRequested => "▲ CHANGES REQ",
            Self::Approved => "✓ APPROVED",
            Self::Done => "✓ DONE",
        }
    }

    pub fn is_completed(self) -> bool {
        matches!(self, Self::Approved | Self::Done)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HerdMessage {
    pub seq: i64,
    pub task_id: String,
    pub sender: String,
    pub sender_agent: String,
    pub verdict: String,
    pub body: String,
    pub round: u32,
    pub created_at: i64,
}

impl HerdMessage {
    pub fn to_json(&self) -> Value {
        json!({
            "seq": self.seq,
            "task_id": self.task_id,
            "sender": self.sender,
            "sender_agent": self.sender_agent,
            "verdict": self.verdict,
            "body": self.body,
            "round": self.round,
            "created_at": self.created_at,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HerdTask {
    pub task_id: String,
    pub parent_terminal_key: String,
    pub child_terminal_key: String,
    pub parent_agent: Option<Agent>,
    pub child_agent: Agent,
    pub role: TaskRole,
    pub goal: String,
    pub status: TaskStatus,
    pub round: u32,
    pub cwd: PathBuf,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_message: Option<HerdMessage>,
}

impl HerdTask {
    pub fn to_json(&self, messages: Option<&[HerdMessage]>) -> Value {
        let mut obj = json!({
            "task_id": self.task_id,
            "parent_terminal_key": self.parent_terminal_key,
            "child_terminal_key": self.child_terminal_key,
            "parent_agent": self.parent_agent.map(Agent::name),
            "child_agent": self.child_agent.name(),
            "role": self.role.as_str(),
            "goal": self.goal,
            "status": self.status.as_str(),
            "round": self.round,
            "cwd": self.cwd,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "last_message": self.last_message.as_ref().map(HerdMessage::to_json),
        });
        if let Some(msgs) = messages {
            obj["messages"] = Value::Array(msgs.iter().map(HerdMessage::to_json).collect());
        }
        obj
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn generate_task_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = TASK_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = u64::from(std::process::id());
    let mix = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(pid.rotate_left(17))
        .wrapping_add(seq);
    format!("t-{:08x}", (mix as u32) ^ ((mix >> 32) as u32))
}

pub fn generate_synthetic_terminal_key() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = TASK_ID_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = u64::from(std::process::id());
    let hi = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(pid.rotate_left(21));
    let lo = nanos
        .rotate_left(31)
        .wrapping_mul(0xBF58_476D_1CE4_E5B9)
        .wrapping_add(seq);
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (hi >> 32) as u32,
        ((hi >> 16) & 0xffff) as u16,
        (hi & 0x0fff) as u16,
        ((lo >> 48) & 0x0fff) as u16,
        lo & 0xffff_ffff_ffff
    )
}

pub fn recommended_child_agent(parent: Option<Agent>) -> Agent {
    match parent {
        Some(Agent::Codex) => Agent::Claude,
        Some(Agent::Claude | Agent::Grok | Agent::Agy | Agent::Opencode | Agent::Pi) | None => {
            Agent::Codex
        }
    }
}

fn sanitize_text(input: &str, max_chars: usize) -> String {
    input
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[allow(clippy::too_many_arguments)]
pub fn create_task(
    store: &Store,
    task_id: &str,
    parent_terminal_key: &str,
    child_terminal_key: &str,
    parent_agent: Option<Agent>,
    child_agent: Agent,
    role: TaskRole,
    goal: &str,
    cwd: &Path,
) -> Result<(HerdTask, i64)> {
    let clean_goal = sanitize_text(goal, 8192);
    if clean_goal.is_empty() {
        return Err(invalid("task goal/prompt must not be empty"));
    }
    let parent_key = canonical_terminal_key(parent_terminal_key);
    let child_key = canonical_terminal_key(child_terminal_key);
    let now = unix_now();
    let parent_agent_str = parent_agent.map(Agent::name).unwrap_or("shell");
    let cwd_str = cwd
        .to_str()
        .ok_or_else(|| invalid("task working directory is not valid UTF-8"))?;

    let db = store
        .connect(true)?
        .ok_or_else(|| invalid("state database unavailable"))?;
    db.execute("BEGIN IMMEDIATE", [])?;
    let res = (|| -> Result<i64> {
        db.execute(
            "INSERT INTO herd_tasks (
                task_id, parent_terminal_key, child_terminal_key, parent_agent, child_agent,
                role, goal, status, round, cwd, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?10, ?10)",
            params![
                task_id,
                parent_key,
                child_key,
                parent_agent.map(Agent::name).unwrap_or(""),
                child_agent.name(),
                role.as_str(),
                clean_goal,
                TaskStatus::WaitingChild.as_str(),
                cwd_str,
                now,
            ],
        )?;
        db.execute(
            "INSERT INTO herd_messages (
                task_id, sender, sender_agent, verdict, body, round, created_at
            ) VALUES (?1, 'parent', ?2, '', ?3, 1, ?4)",
            params![task_id, parent_agent_str, clean_goal, now],
        )?;
        Ok(db.last_insert_rowid())
    })();
    match res {
        Ok(seq) => {
            db.execute("COMMIT", [])?;
            let msg = HerdMessage {
                seq,
                task_id: task_id.to_owned(),
                sender: "parent".into(),
                sender_agent: parent_agent_str.into(),
                verdict: String::new(),
                body: clean_goal.clone(),
                round: 1,
                created_at: now,
            };
            Ok((
                HerdTask {
                    task_id: task_id.to_owned(),
                    parent_terminal_key: parent_key,
                    child_terminal_key: child_key,
                    parent_agent,
                    child_agent,
                    role,
                    goal: clean_goal,
                    status: TaskStatus::WaitingChild,
                    round: 1,
                    cwd: cwd.to_path_buf(),
                    created_at: now,
                    updated_at: now,
                    last_message: Some(msg),
                },
                seq,
            ))
        }
        Err(e) => {
            let _ = db.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

pub fn record_parent_message(store: &Store, task_id: &str, body: &str) -> Result<(HerdTask, i64)> {
    let clean_body = sanitize_text(body, 16384);
    if clean_body.is_empty() {
        return Err(invalid("message body must not be empty"));
    }
    let now = unix_now();
    let db = store
        .connect(true)?
        .ok_or_else(|| invalid("state database unavailable"))?;
    db.execute("BEGIN IMMEDIATE", [])?;
    let res = (|| -> Result<i64> {
        let (parent_agent_str, current_round): (String, u32) = db
            .query_row(
                "SELECT parent_agent, round FROM herd_tasks WHERE task_id = ?1",
                params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| invalid(format!("unknown herd task: {task_id}")))?;

        // Idempotency guard: if the parent retries the exact same message after a wait timeout,
        // do not duplicate the message or overwrite an already-arrived child reply.
        let mut last_stmt = db.prepare(
            "SELECT seq, sender, body FROM herd_messages WHERE task_id = ?1 ORDER BY seq DESC LIMIT 2",
        )?;
        let recent: Vec<(i64, String, String)> = last_stmt
            .query_map(params![task_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(last_stmt);
        if let Some((last_seq, last_sender, last_body)) = recent.first() {
            if last_sender == "parent" && last_body == &clean_body {
                return Ok(*last_seq);
            }
            if last_sender == "child"
                && let Some((prev_seq, prev_sender, prev_body)) = recent.get(1)
                && prev_sender == "parent"
                && prev_body == &clean_body
            {
                return Ok(*prev_seq);
            }
        }

        let next_round = current_round.saturating_add(1);
        let sender_agent = if parent_agent_str.is_empty() {
            "parent".to_owned()
        } else {
            parent_agent_str
        };
        db.execute(
            "UPDATE herd_tasks SET status = ?1, round = ?2, updated_at = ?3 WHERE task_id = ?4",
            params![TaskStatus::WaitingChild.as_str(), next_round, now, task_id],
        )?;
        db.execute(
            "INSERT INTO herd_messages (
                task_id, sender, sender_agent, verdict, body, round, created_at
            ) VALUES (?1, 'parent', ?2, '', ?3, ?4, ?5)",
            params![task_id, sender_agent, clean_body, next_round, now],
        )?;
        Ok(db.last_insert_rowid())
    })();
    match res {
        Ok(seq) => {
            db.execute("COMMIT", [])?;
            let (task, _) = get_task(store, task_id)?
                .ok_or_else(|| invalid(format!("unknown herd task: {task_id}")))?;
            Ok((task, seq))
        }
        Err(e) => {
            let _ = db.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

pub fn record_child_reply(
    store: &Store,
    task_id: &str,
    verdict: ReplyVerdict,
    body: &str,
) -> Result<(HerdTask, i64)> {
    let clean_body = sanitize_text(body, 16384);
    if clean_body.is_empty() {
        return Err(invalid("reply message body must not be empty"));
    }
    let now = unix_now();
    let next_status = match verdict {
        ReplyVerdict::ChangesRequested => TaskStatus::ChangesRequested,
        ReplyVerdict::Approved => TaskStatus::Approved,
        ReplyVerdict::Done => TaskStatus::Done,
        ReplyVerdict::Comment => TaskStatus::WaitingParent,
    };
    let db = store
        .connect(true)?
        .ok_or_else(|| invalid("state database unavailable"))?;
    db.execute("BEGIN IMMEDIATE", [])?;
    let res = (|| -> Result<i64> {
        let (child_agent_str, round): (String, u32) = db
            .query_row(
                "SELECT child_agent, round FROM herd_tasks WHERE task_id = ?1",
                params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| invalid(format!("unknown herd task: {task_id}")))?;

        // Idempotency guard: if the child retries the exact same reply after a bash timeout,
        // do not duplicate the row or clobber a newer parent round that arrived in the meantime.
        let mut last_stmt = db.prepare(
            "SELECT seq, sender, verdict, body FROM herd_messages WHERE task_id = ?1 ORDER BY seq DESC LIMIT 2",
        )?;
        let recent: Vec<(i64, String, String, String)> = last_stmt
            .query_map(params![task_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(last_stmt);
        if let Some((last_seq, last_sender, last_verdict, last_body)) = recent.first() {
            if last_sender == "child"
                && last_verdict == verdict.as_str()
                && last_body == &clean_body
            {
                return Ok(*last_seq);
            }
            if last_sender == "parent"
                && let Some((prev_seq, prev_sender, prev_verdict, prev_body)) = recent.get(1)
                && prev_sender == "child"
                && prev_verdict == verdict.as_str()
                && prev_body == &clean_body
            {
                return Ok(*prev_seq);
            }
        }

        db.execute(
            "UPDATE herd_tasks SET status = ?1, updated_at = ?2 WHERE task_id = ?3",
            params![next_status.as_str(), now, task_id],
        )?;
        db.execute(
            "INSERT INTO herd_messages (
                task_id, sender, sender_agent, verdict, body, round, created_at
            ) VALUES (?1, 'child', ?2, ?3, ?4, ?5, ?6)",
            params![
                task_id,
                child_agent_str,
                verdict.as_str(),
                clean_body,
                round,
                now
            ],
        )?;
        Ok(db.last_insert_rowid())
    })();
    match res {
        Ok(seq) => {
            db.execute("COMMIT", [])?;
            let (task, _) = get_task(store, task_id)?
                .ok_or_else(|| invalid(format!("unknown herd task: {task_id}")))?;
            Ok((task, seq))
        }
        Err(e) => {
            let _ = db.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

pub fn list_tasks(store: &Store) -> Result<Vec<HerdTask>> {
    let Some(db) = store.connect(false)? else {
        return Ok(Vec::new());
    };
    let has_table: bool = db.query_row(
        "SELECT count(*)>0 FROM sqlite_master WHERE type='table' AND name='herd_tasks'",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(Vec::new());
    }
    let mut stmt = db.prepare(
        "SELECT
            t.task_id, t.parent_terminal_key, t.child_terminal_key, t.parent_agent, t.child_agent,
            t.role, t.goal, t.status, t.round, t.cwd, t.created_at, t.updated_at,
            m.seq, m.sender, m.sender_agent, m.verdict, m.body, m.round, m.created_at
         FROM herd_tasks t
         LEFT JOIN herd_messages m ON m.seq = (
            SELECT MAX(seq) FROM herd_messages WHERE task_id = t.task_id
         )
         ORDER BY t.updated_at DESC, COALESCE(m.seq, 0) DESC, t.rowid DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        let task_id: String = row.get(0)?;
        let parent_key: String = row.get(1)?;
        let child_key: String = row.get(2)?;
        let parent_agent_str: String = row.get(3)?;
        let child_agent_str: String = row.get(4)?;
        let role_str: String = row.get(5)?;
        let goal: String = row.get(6)?;
        let status_str: String = row.get(7)?;
        let round: u32 = row.get(8)?;
        let cwd_str: String = row.get(9)?;
        let created_at: i64 = row.get(10)?;
        let updated_at: i64 = row.get(11)?;
        let msg_seq: Option<i64> = row.get(12)?;
        let last_message = if let Some(seq) = msg_seq {
            Some(HerdMessage {
                seq,
                task_id: task_id.clone(),
                sender: row.get(13)?,
                sender_agent: row.get(14)?,
                verdict: row.get(15)?,
                body: row.get(16)?,
                round: row.get(17)?,
                created_at: row.get(18)?,
            })
        } else {
            None
        };
        Ok((
            task_id,
            parent_key,
            child_key,
            parent_agent_str,
            child_agent_str,
            role_str,
            goal,
            status_str,
            round,
            cwd_str,
            created_at,
            updated_at,
            last_message,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (
            task_id,
            parent_terminal_key,
            child_terminal_key,
            parent_agent_str,
            child_agent_str,
            role_str,
            goal,
            status_str,
            round,
            cwd_str,
            created_at,
            updated_at,
            last_message,
        ) = r?;
        let parent_agent = if parent_agent_str.is_empty() {
            None
        } else {
            Agent::parse(&parent_agent_str).ok()
        };
        let Ok(child_agent) = Agent::parse(&child_agent_str) else {
            continue;
        };
        out.push(HerdTask {
            task_id,
            parent_terminal_key,
            child_terminal_key,
            parent_agent,
            child_agent,
            role: TaskRole::parse(&role_str),
            goal,
            status: TaskStatus::parse(&status_str),
            round,
            cwd: PathBuf::from(cwd_str),
            created_at,
            updated_at,
            last_message,
        });
    }
    Ok(out)
}

pub fn get_task(store: &Store, task_id: &str) -> Result<Option<(HerdTask, Vec<HerdMessage>)>> {
    let tasks = list_tasks(store)?;
    let Some(task) = tasks.into_iter().find(|t| t.task_id == task_id) else {
        return Ok(None);
    };
    let Some(db) = store.connect(false)? else {
        return Ok(Some((task, Vec::new())));
    };
    let mut stmt = db.prepare(
        "SELECT seq, task_id, sender, sender_agent, verdict, body, round, created_at
         FROM herd_messages
         WHERE task_id = ?1
         ORDER BY seq ASC",
    )?;
    let rows = stmt.query_map(params![task_id], |row| {
        Ok(HerdMessage {
            seq: row.get(0)?,
            task_id: row.get(1)?,
            sender: row.get(2)?,
            sender_agent: row.get(3)?,
            verdict: row.get(4)?,
            body: row.get(5)?,
            round: row.get(6)?,
            created_at: row.get(7)?,
        })
    })?;
    let mut messages = Vec::new();
    for m in rows {
        messages.push(m?);
    }
    Ok(Some((task, messages)))
}

pub fn resolve_caller_terminal_key(store: &Store) -> Option<String> {
    if let Ok(k) = std::env::var("GHOSTBACK_TERMINAL_KEY")
        && !k.trim().is_empty()
    {
        return Some(canonical_terminal_key(&k));
    }
    if std::env::var_os("GHOSTBACK_TASK_ID")
        .filter(|v| !v.is_empty())
        .is_none()
        && let Ok(k) = std::env::var("GHOSTBACK_PARENT_KEY")
        && !k.trim().is_empty()
    {
        return Some(canonical_terminal_key(&k));
    }
    let (ancestor_pids, ancestor_tty) = macos::ancestor_pids_and_tty(std::process::id());
    if let Ok(bindings) = store.bindings() {
        for pid in &ancestor_pids {
            let prefix = format!("{pid}:");
            if let Some(b) = bindings
                .iter()
                .find(|b| b.owner_key.starts_with(&prefix) || b.shell_key.starts_with(&prefix))
            {
                return Some(canonical_terminal_key(&b.session.terminal_key));
            }
        }
    }
    if let Ok(k) = macos::current_terminal_key() {
        return Some(canonical_terminal_key(&k));
    }
    if let Some(tty) = ancestor_tty
        && let Ok(inv) = macos::inventory()
        && let Ok(uuid) = macos::match_terminal(&inv, &tty)
    {
        return Some(uuid.to_string());
    }
    None
}

pub fn resolve_terminal_agent(store: &Store, terminal_key: &str) -> Option<Agent> {
    if let Ok(bindings) = store.bindings()
        && let Some(b) = bindings
            .iter()
            .find(|b| b.session.terminal_key.eq_ignore_ascii_case(terminal_key))
    {
        return Some(b.session.agent);
    }
    if let Ok(sessions) = store.list()
        && let Some(s) = sessions
            .iter()
            .find(|s| s.terminal_key.eq_ignore_ascii_case(terminal_key))
    {
        return Some(s.agent);
    }
    if let Ok(raw) = std::env::var("GHOSTBACK_PARENT_AGENT")
        && let Ok(agent) = Agent::parse(raw.trim())
    {
        return Some(agent);
    }
    None
}

pub fn resolve_task_id(
    store: &Store,
    explicit_task: Option<&str>,
    terminal_key: Option<&str>,
    as_child: bool,
) -> Result<String> {
    if let Some(id) = explicit_task.filter(|s| !s.trim().is_empty()) {
        return Ok(id.trim().to_owned());
    }
    if as_child
        && let Ok(env_id) = std::env::var("GHOSTBACK_TASK_ID")
        && !env_id.trim().is_empty()
    {
        return Ok(env_id.trim().to_owned());
    }
    let resolved_tkey = terminal_key
        .map(str::to_owned)
        .or_else(|| resolve_caller_terminal_key(store));
    let tasks = list_tasks(store)?;
    if let Some(tkey) = resolved_tkey.as_deref() {
        let canon = canonical_terminal_key(tkey);
        let matches_terminal = |t: &HerdTask| {
            if as_child {
                t.child_terminal_key.eq_ignore_ascii_case(&canon)
            } else {
                t.parent_terminal_key.eq_ignore_ascii_case(&canon)
            }
        };
        if let Some(active) = tasks
            .iter()
            .find(|t| matches_terminal(t) && !t.status.is_completed())
        {
            return Ok(active.task_id.clone());
        }
        if let Some(found) = tasks.iter().find(|t| matches_terminal(t)) {
            return Ok(found.task_id.clone());
        }
    }
    if let Ok(env_id) = std::env::var("GHOSTBACK_TASK_ID")
        && !env_id.trim().is_empty()
    {
        return Ok(env_id.trim().to_owned());
    }
    if let Some(active) = tasks.iter().find(|t| !t.status.is_completed()) {
        return Ok(active.task_id.clone());
    }
    if let Some(first) = tasks.first() {
        return Ok(first.task_id.clone());
    }
    Err(invalid(
        "could not resolve an active Herd task; pass --task <task_id>",
    ))
}

pub fn wait_for_turn(
    store: &Store,
    task_id: &str,
    after_seq: i64,
    expected_sender: &str,
    timeout: Duration,
) -> Result<(HerdTask, HerdMessage)> {
    let started = Instant::now();
    loop {
        if let Some((task, messages)) = get_task(store, task_id)?
            && let Some(reply) = messages
                .into_iter()
                .find(|m| m.seq > after_seq && m.sender == expected_sender)
        {
            return Ok((task, reply));
        }
        if started.elapsed() >= timeout {
            return Err(invalid(format!(
                "timed out after {}s waiting for {expected_sender} on task {task_id}",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(220));
    }
}

/// Waits for the peer (`expected_sender` = `"parent"` or `"child"`) to send the latest message on `task_id`.
/// If the most recent message on `task_id` is already from `expected_sender`, returns it immediately.
pub fn wait_for_peer_or_latest(
    store: &Store,
    task_id: &str,
    expected_sender: &str,
    timeout: Duration,
) -> Result<(HerdTask, HerdMessage)> {
    let (_, messages) = get_task(store, task_id)?
        .ok_or_else(|| invalid(format!("unknown herd task: {task_id}")))?;
    if let Some(last) = messages.last()
        && last.sender == expected_sender
    {
        let (task, _) = get_task(store, task_id)?
            .ok_or_else(|| invalid(format!("unknown herd task: {task_id}")))?;
        return Ok((task, last.clone()));
    }
    let after_seq = messages.last().map(|m| m.seq).unwrap_or(0);
    wait_for_turn(store, task_id, after_seq, expected_sender, timeout)
}

pub fn format_wait_timeout(
    exe: &Path,
    state_dir: &Path,
    task: &HerdTask,
    expected_sender: &str,
) -> String {
    let gb_cmd = format!(
        "{} --state-dir {}",
        shell_quote(&exe.to_string_lossy()),
        shell_quote(&state_dir.to_string_lossy())
    );
    let peer_label = if expected_sender == "parent" {
        task.parent_agent
            .map(|a| a.name().to_ascii_uppercase())
            .unwrap_or_else(|| "PARENT".into())
    } else {
        task.child_agent.name().to_ascii_uppercase()
    };
    format!(
        "[Ghostback Herd · Task {} · Round {} · Still waiting for {}]\n\
         Status: {} is still working on Round {} (task status: {}).\n\
         Next step: Continue waiting for {}'s response by calling the `ghostback_wait` MCP tool (`task_id: \"{}\"`, `for_sender: \"{}\"`) or running:\n  \
         {gb_cmd} wait --task {} --for {}\n",
        task.task_id,
        task.round,
        peer_label,
        peer_label,
        task.round,
        task.status.as_str(),
        peer_label,
        task.task_id,
        expected_sender,
        task.task_id,
        expected_sender
    )
}

pub fn format_reply_for_parent_with_cmd(
    exe: &Path,
    state_dir: &Path,
    task: &HerdTask,
    msg: &HerdMessage,
) -> String {
    let gb_cmd = format!(
        "{} --state-dir {}",
        shell_quote(&exe.to_string_lossy()),
        shell_quote(&state_dir.to_string_lossy())
    );
    let verdict = if msg.verdict.is_empty() {
        task.status.as_str().to_ascii_uppercase()
    } else {
        msg.verdict.to_ascii_uppercase()
    };
    let child_name = task.child_agent.name().to_ascii_uppercase();
    let role_name = task.role.label().to_ascii_uppercase();
    let mut out = format!(
        "[Ghostback Herd · Task {} · Round {} Reply from {} ({})]\nVerdict: {}\n\n{}\n",
        task.task_id, msg.round, child_name, role_name, verdict, msg.body
    );
    if !task.status.is_completed() {
        out.push_str(&format!(
            "\nNext step: Address the feedback above, then send your revision to {child_name} and wait for the next round via the `ghostback_ask` MCP tool (`task_id: \"{}\"`, `wait: true`) or:\n  {gb_cmd} ask --task {} --wait \"<summary of fixes made>\"\n",
            task.task_id, task.task_id
        ));
    } else {
        out.push_str(&format!(
            "\nTask {} is marked {} — collaboration loop complete.\n",
            task.task_id, verdict
        ));
    }
    out
}

pub fn format_reply_for_parent(task: &HerdTask, msg: &HerdMessage) -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
    let state_dir = std::env::var_os("GHOSTBACK_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| crate::sessions::default_state_dir().ok())
        .unwrap_or_else(|| PathBuf::from(".ghostback"));
    format_reply_for_parent_with_cmd(&exe, &state_dir, task, msg)
}

pub fn format_ask_for_child_with_cmd(
    exe: &Path,
    state_dir: &Path,
    task: &HerdTask,
    msg: &HerdMessage,
) -> String {
    let gb_cmd = format!(
        "{} --state-dir {}",
        shell_quote(&exe.to_string_lossy()),
        shell_quote(&state_dir.to_string_lossy())
    );
    let parent_name = task
        .parent_agent
        .map(|a| a.name().to_ascii_uppercase())
        .unwrap_or_else(|| "PARENT".into());
    format!(
        "[Ghostback Herd · Task {} · Round {} Update from {}]\n\n{}\n\n\
         Next step: Re-verify the workspace changes and respond via the `ghostback_reply` MCP tool or CLI:\n  \
         {gb_cmd} reply --task {} --status changes_requested --wait \"<findings>\"\n  \
         # or when ready to sign off:\n  \
         {gb_cmd} reply --task {} --status approved \"<final approval>\"\n",
        task.task_id, msg.round, parent_name, msg.body, task.task_id, task.task_id
    )
}

pub fn format_ask_for_child(task: &HerdTask, msg: &HerdMessage) -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
    let state_dir = std::env::var_os("GHOSTBACK_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| crate::sessions::default_state_dir().ok())
        .unwrap_or_else(|| PathBuf::from(".ghostback"));
    format_ask_for_child_with_cmd(&exe, &state_dir, task, msg)
}

/// High-level specification for delegating a task to a Child agent harness.
#[derive(Clone, Debug)]
pub struct DelegateSpec<'a> {
    pub exe: &'a Path,
    pub parent_key: Option<&'a str>,
    pub parent_agent: Option<Agent>,
    pub target_uuid: Option<uuid::Uuid>,
    pub child_agent: Agent,
    pub role: TaskRole,
    pub raw_prompt: &'a str,
    pub options: AgentLaunchOptions,
    pub cwd: &'a Path,
    pub no_spawn: bool,
    pub sidebar_cmd: Option<&'a str>,
}

/// Unified pipeline for spawning a Child agent surface, writing per-task MCP configuration,
/// and recording the initial Round 1 task and message in SQLite.
pub fn delegate_task(
    store: &Store,
    spec: &DelegateSpec<'_>,
) -> Result<(HerdTask, i64, Option<uuid::Uuid>)> {
    let parent_key = spec
        .parent_key
        .filter(|s| !s.trim().is_empty())
        .map(canonical_terminal_key)
        .or_else(|| resolve_caller_terminal_key(store))
        .or_else(|| std::env::var("GHOSTBACK_PARENT_KEY").ok())
        .unwrap_or_else(generate_synthetic_terminal_key);
    let parent_agent = spec
        .parent_agent
        .or_else(|| resolve_terminal_agent(store, &parent_key));

    let (inline_opts, cleaned_prompt) =
        extract_inline_launch_options_for_role(spec.raw_prompt, spec.role);
    let prompt = if cleaned_prompt.trim().is_empty() {
        spec.role.default_goal()
    } else {
        cleaned_prompt.trim()
    };
    let merged_opts = spec.options.clone().merged_with_inline(inline_opts);

    let task_id = generate_task_id();
    let _ = ensure_mcp_config_file_for_task(
        spec.exe,
        &store.directory,
        Some(&task_id),
        Some(&parent_key),
    );
    let launch_cmd = build_child_launch_command_with_options(
        spec.exe,
        &store.directory,
        &task_id,
        &parent_key,
        parent_agent,
        spec.child_agent,
        spec.role,
        prompt,
        &merged_opts,
    );

    let (child_key, child_uuid) = if spec.no_spawn {
        (generate_synthetic_terminal_key(), None)
    } else {
        let target_uuid = spec
            .target_uuid
            .or_else(|| uuid::Uuid::parse_str(&parent_key).ok());
        let disposition = if spec.sidebar_cmd.is_some() {
            macos::SpawnDisposition::SidebarRight
        } else {
            macos::SpawnDisposition::SplitRight
        };
        let spawned_uuid =
            macos::spawn_surface(target_uuid, disposition, spec.cwd, Some(&launch_cmd))?;
        let title = format_child_terminal_title(spec.role, spec.child_agent, &task_id);
        let _ = macos::set_terminal_title(spawned_uuid, &title);
        (spawned_uuid.to_string(), Some(spawned_uuid))
    };

    let (task, seq) = create_task(
        store,
        &task_id,
        &parent_key,
        &child_key,
        parent_agent,
        spec.child_agent,
        spec.role,
        prompt,
        spec.cwd,
    )?;
    Ok((task, seq, child_uuid))
}

/// Waits for a Child reply after `after_seq`, returning either the formatted reply or a recoverable wait-timeout guidance message.
pub fn wait_or_timeout_for_parent(
    store: &Store,
    exe: &Path,
    task: HerdTask,
    after_seq: i64,
    timeout: Duration,
) -> Result<String> {
    match wait_for_turn(store, &task.task_id, after_seq, "child", timeout) {
        Ok((updated, reply)) => Ok(format_reply_for_parent_with_cmd(
            exe,
            &store.directory,
            &updated,
            &reply,
        )),
        Err(_) => {
            let latest_task = get_task(store, &task.task_id)?
                .map(|(t, _)| t)
                .unwrap_or(task);
            Ok(format_wait_timeout(
                exe,
                &store.directory,
                &latest_task,
                "child",
            ))
        }
    }
}

/// Waits for a Parent revision after `after_seq`, returning either the formatted update or a recoverable wait-timeout guidance message.
pub fn wait_or_timeout_for_child(
    store: &Store,
    exe: &Path,
    task: HerdTask,
    after_seq: i64,
    timeout: Duration,
) -> Result<String> {
    match wait_for_turn(store, &task.task_id, after_seq, "parent", timeout) {
        Ok((updated, reply)) => Ok(format_ask_for_child_with_cmd(
            exe,
            &store.directory,
            &updated,
            &reply,
        )),
        Err(_) => {
            let latest_task = get_task(store, &task.task_id)?
                .map(|(t, _)| t)
                .unwrap_or(task);
            Ok(format_wait_timeout(
                exe,
                &store.directory,
                &latest_task,
                "parent",
            ))
        }
    }
}

/// Waits for the peer (`expected_sender` = `"child"` or `"parent"`) to respond (or returns its latest message immediately),
/// falling back to a recoverable wait-timeout guidance message if the timeout elapses.
pub fn wait_for_peer_formatted(
    store: &Store,
    exe: &Path,
    task_id: &str,
    expected_sender: &str,
    timeout: Duration,
) -> Result<String> {
    match wait_for_peer_or_latest(store, task_id, expected_sender, timeout) {
        Ok((updated, msg)) => {
            if expected_sender == "child" {
                Ok(format_reply_for_parent_with_cmd(
                    exe,
                    &store.directory,
                    &updated,
                    &msg,
                ))
            } else {
                Ok(format_ask_for_child_with_cmd(
                    exe,
                    &store.directory,
                    &updated,
                    &msg,
                ))
            }
        }
        Err(_) => {
            let (latest_task, _) = get_task(store, task_id)?
                .ok_or_else(|| invalid(format!("unknown herd task: {task_id}")))?;
            Ok(format_wait_timeout(
                exe,
                &store.directory,
                &latest_task,
                expected_sender,
            ))
        }
    }
}
