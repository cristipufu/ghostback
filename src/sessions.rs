use crate::{Result, invalid};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Agy,
    Codex,
    Claude,
    Grok,
    Pi,
    Opencode,
}

impl Agent {
    pub fn name(self) -> &'static str {
        match self {
            Self::Agy => "agy",
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Grok => "grok",
            Self::Pi => "pi",
            Self::Opencode => "opencode",
        }
    }

    pub fn resume_flag(self) -> &'static str {
        match self {
            Self::Agy => "--conversation",
            Self::Codex => "resume",
            Self::Claude | Self::Grok => "--resume",
            Self::Pi | Self::Opencode => "--session",
        }
    }

    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "agy" => Ok(Self::Agy),
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "grok" => Ok(Self::Grok),
            "pi" => Ok(Self::Pi),
            "opencode" => Ok(Self::Opencode),
            _ => Err(invalid(format!("unsupported agent: {name}"))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(agent: Agent, value: &str) -> Result<Self> {
        let value = match agent {
            Agent::Pi | Agent::Opencode => {
                if value.is_empty()
                    || value.len() > 256
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    || !value.as_bytes()[0].is_ascii_alphanumeric()
                    || !value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
                    || (agent == Agent::Opencode
                        && (!value.starts_with("ses_") || value.len() <= 4))
                {
                    return Err(invalid("invalid agent conversation ID"));
                }
                value.to_owned()
            }
            _ => {
                let id = Uuid::parse_str(value)
                    .map_err(|_| invalid("session ID must be an exact conversation UUID"))?;
                if id.is_nil() {
                    return Err(invalid("conversation UUID cannot be nil"));
                }
                id.to_string()
            }
        };
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl From<Uuid> for SessionId {
    fn from(id: Uuid) -> Self {
        Self(id.to_string())
    }
}
impl PartialEq<Uuid> for SessionId {
    fn eq(&self, id: &Uuid) -> bool {
        self.0 == id.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub terminal_key: String,
    pub agent: Agent,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<PathBuf>,
    pub cwd: PathBuf,
}

impl Session {
    pub fn new(terminal_key: String, agent: Agent, session_id: &str, cwd: PathBuf) -> Result<Self> {
        let session = Self {
            terminal_key: canonical_terminal_key(&terminal_key),
            agent,
            session_id: SessionId::new(agent, session_id)?,
            session_file: None,
            cwd,
        };
        session.validate()?;
        Ok(session)
    }

    pub fn validate(&self) -> Result<()> {
        if SessionId::new(self.agent, self.session_id.as_str())? != self.session_id {
            return Err(invalid("conversation ID is not canonical"));
        }
        if let Some(file) = &self.session_file
            && (self.agent != Agent::Pi
                || !file.is_absolute()
                || file
                    .to_str()
                    .is_none_or(|s| s.len() > 8192 || s.chars().any(char::is_control)))
        {
            return Err(invalid("invalid Pi session file path"));
        }
        if self.terminal_key.trim().is_empty()
            || self.terminal_key.len() > 256
            || self.terminal_key.chars().any(char::is_control)
        {
            return Err(invalid("a valid native terminal key is required"));
        }
        let cwd = self
            .cwd
            .to_str()
            .ok_or_else(|| invalid("directory is not valid UTF-8"))?;
        if !self.cwd.is_absolute() || cwd.chars().any(char::is_control) {
            return Err(invalid(
                "working directory must be absolute and contain no control characters",
            ));
        }
        Ok(())
    }

    /// Pi creates a fresh conversation for a missing path; never launch until its header matches.
    pub fn validate_resume(&self) -> Result<()> {
        self.validate()?;
        if self.agent != Agent::Pi {
            return Ok(());
        }
        use std::io::{BufRead, Read};
        let file = self
            .session_file
            .as_ref()
            .ok_or_else(|| invalid("Pi recovery requires its exact session file"))?;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let input = options.open(file).map_err(|e| {
            invalid(format!(
                "Pi session file is unavailable: {e}. Restore the file before retrying."
            ))
        })?;
        if !input.metadata()?.is_file() {
            return Err(invalid("Pi session path is not a regular file"));
        }
        let mut header = String::new();
        std::io::BufReader::new(input.take(65537)).read_line(&mut header)?;
        if header.len() > 65536 {
            return Err(invalid("Pi session header exceeds 64 KiB"));
        }
        let header: serde_json::Value = serde_json::from_str(&header)?;
        let directory = header
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        let same_directory = directory.as_ref().is_some_and(|p| {
            p == &self.cwd
                || p.canonicalize()
                    .ok()
                    .zip(self.cwd.canonicalize().ok())
                    .is_some_and(|(a, b)| a == b)
        });
        if header.get("type").and_then(|v| v.as_str()) != Some("session")
            || header.get("id").and_then(|v| v.as_str()) != Some(self.session_id.as_str())
            || !same_directory
        {
            return Err(invalid(
                "Pi session file does not match the saved conversation and directory",
            ));
        }
        Ok(())
    }

    pub fn argv(&self) -> Vec<String> {
        vec![
            self.agent.name().into(),
            self.agent.resume_flag().into(),
            self.session_file
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.session_id.to_string()),
        ]
    }

    pub fn command(&self) -> Result<String> {
        self.validate()?;
        let cwd = self
            .cwd
            .to_str()
            .ok_or_else(|| invalid("directory is not valid UTF-8"))?;
        Ok(format!(
            "cd -- {} && {}",
            shell_quote(cwd),
            self.argv()
                .iter()
                .map(|arg| shell_quote(arg))
                .collect::<Vec<_>>()
                .join(" ")
        ))
    }
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub(crate) fn canonical_terminal_key(key: &str) -> String {
    Uuid::parse_str(key)
        .map(|id| id.to_string())
        .unwrap_or_else(|_| key.to_owned())
}

pub struct Store {
    pub directory: PathBuf,
}

fn sqlite_busy(error: &crate::Error) -> bool {
    matches!(
        error.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(code, _))
            if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

impl Store {
    pub fn new(directory: PathBuf) -> Result<Self> {
        let directory = if directory.is_absolute() {
            directory
        } else {
            std::env::current_dir()?.join(directory)
        };
        Ok(Self { directory })
    }

    pub fn path(&self) -> PathBuf {
        self.directory.join("sessions.sqlite3")
    }

    pub(crate) fn connect(&self, write: bool) -> Result<Option<Connection>> {
        let started = Instant::now();
        loop {
            let remaining = Duration::from_secs(10).saturating_sub(started.elapsed());
            match self.connect_once(write, remaining) {
                Err(error) if remaining > Duration::from_millis(10) && sqlite_busy(&error) => {
                    // WAL initialization can return BUSY without invoking SQLite's
                    // busy handler. Retry the entire connection; Drop rolls back
                    // any partially initialized transaction before another attempt.
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => return result,
            }
        }
    }

    fn connect_once(&self, write: bool, timeout: Duration) -> Result<Option<Connection>> {
        if !write && !self.path().try_exists()? {
            return Ok(None);
        }
        if write {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&self.directory)?;
        }
        let flags = if write {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        let db = Connection::open_with_flags(self.path(), flags)?;
        db.busy_timeout(timeout)?;
        let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 3 || (!write && version == 0) {
            return Err(invalid(format!("unsupported state schema: {version}")));
        }
        if write {
            db.pragma_update(None, "journal_mode", "WAL")?;
            db.pragma_update(None, "synchronous", "FULL")?;
            db.execute_batch(
                "BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS sessions (
                    terminal_key TEXT PRIMARY KEY,
                    agent TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    cwd TEXT NOT NULL,
                    recorded_at TEXT NOT NULL,
                    session_file TEXT
                );
                CREATE TABLE IF NOT EXISTS runtime (
                    terminal_key TEXT PRIMARY KEY,
                    shell_key TEXT NOT NULL,
                    owner_key TEXT NOT NULL,
                    executable TEXT NOT NULL,
                    options_json TEXT NOT NULL,
                    phase TEXT NOT NULL,
                    reason TEXT NOT NULL DEFAULT '',
                    claim_shell TEXT NOT NULL DEFAULT '',
                    seen_at INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS observations (
                    terminal_key TEXT PRIMARY KEY, owner_key TEXT NOT NULL, tick TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS health (
                    key TEXT PRIMARY KEY, value TEXT NOT NULL
                );
                ",
            )?;
            let has_file: bool = db.query_row(
                "SELECT count(*)>0 FROM pragma_table_info('sessions') WHERE name='session_file'",
                [],
                |row| row.get(0),
            )?;
            if !has_file {
                db.execute_batch("ALTER TABLE sessions ADD COLUMN session_file TEXT;")?;
            }
            db.pragma_update(None, "user_version", 3)?;
            db.execute_batch("COMMIT;")?;
        }
        Ok(Some(db))
    }

    pub fn record(&self, session: &Session) -> Result<()> {
        session.validate()?;
        let db = self
            .connect(true)?
            .ok_or_else(|| invalid("state database unavailable"))?;
        persist_session(&db, session)
    }

    pub fn list(&self) -> Result<Vec<Session>> {
        let Some(db) = self.connect(false)? else {
            return Ok(Vec::new());
        };
        list_sessions(&db)
    }

    pub fn forget(&self, terminal_key: &str, session_id: &str) -> Result<bool> {
        if !self.path().try_exists()? {
            return Ok(false);
        }
        let mut db = self
            .connect(true)?
            .ok_or_else(|| invalid("state database unavailable"))?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        use rusqlite::OptionalExtension;
        let agent = tx
            .query_row(
                "SELECT agent FROM sessions WHERE terminal_key=?1",
                [canonical_terminal_key(terminal_key)],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(agent) = agent else {
            return Ok(false);
        };
        let id = SessionId::new(Agent::parse(&agent)?, session_id)?;
        let removed = tx.execute(
            "DELETE FROM sessions WHERE terminal_key=?1 AND session_id=?2",
            params![canonical_terminal_key(terminal_key), id.to_string()],
        )? > 0;
        if removed {
            tx.execute(
                "DELETE FROM observations WHERE terminal_key=?1",
                [canonical_terminal_key(terminal_key)],
            )?;
            tx.execute(
                "DELETE FROM runtime WHERE terminal_key=?1",
                [canonical_terminal_key(terminal_key)],
            )?;
        }
        tx.commit()?;
        Ok(removed)
    }
}

pub(crate) fn persist_session(db: &Connection, session: &Session) -> Result<()> {
    session.validate()?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .to_string();
    db.execute(
        "INSERT INTO sessions (terminal_key,agent,session_id,cwd,recorded_at,session_file) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(terminal_key) DO UPDATE SET
                agent=excluded.agent, session_id=excluded.session_id,
                cwd=excluded.cwd, recorded_at=excluded.recorded_at,session_file=excluded.session_file",
        params![
            canonical_terminal_key(&session.terminal_key),
            session.agent.name(),
            session.session_id.to_string(),
            session.cwd.to_str(),
            timestamp,
            session.session_file.as_ref().and_then(|p|p.to_str())
        ],
    )?;
    Ok(())
}

pub(crate) fn list_sessions(db: &Connection) -> Result<Vec<Session>> {
    let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let sql = if version >= 3 {
        "SELECT terminal_key,agent,session_id,cwd,session_file FROM sessions ORDER BY terminal_key"
    } else {
        "SELECT terminal_key,agent,session_id,cwd,NULL FROM sessions ORDER BY terminal_key"
    };
    let mut statement = db.prepare(sql)?;
    let mut rows = statement.query([])?;
    let mut sessions = Vec::new();
    while let Some(row) = rows.next()? {
        let agent: String = row.get(1)?;
        let id: String = row.get(2)?;
        let mut session = Session::new(
            row.get(0)?,
            Agent::parse(&agent)?,
            &id,
            PathBuf::from(row.get::<_, String>(3)?),
        )?;
        session.session_file = row.get::<_, Option<String>>(4)?.map(PathBuf::from);
        session.validate()?;
        sessions.push(session);
    }
    Ok(sessions)
}

pub fn default_state_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("GHOSTBACK_STATE_DIR").filter(|p| !p.is_empty()) {
        return Ok(path.into());
    }
    let home = || {
        std::env::var_os("HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| invalid("cannot determine home directory; use --state-dir"))
    };
    if cfg!(target_os = "macos") {
        return Ok(home()?.join("Library/Application Support/Ghostback"));
    }
    if let Some(path) = std::env::var_os("XDG_STATE_HOME").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path).join("ghostback"));
    }
    Ok(home()?.join(".local/state/ghostback"))
}

pub fn hook_session(
    agent: Agent,
    payload: serde_json::Value,
    terminal_key: &str,
) -> Result<Session> {
    let object = payload
        .as_object()
        .ok_or_else(|| invalid("hook input must be a JSON object"))?;
    if object.get("hook_event_name").and_then(|v| v.as_str()) == Some("SessionEnd") {
        return Err(invalid("SessionEnd events cannot register a conversation"));
    }
    if agent == Agent::Grok {
        return Err(invalid(
            "Grok is captured through its process registry; it does not use lifecycle hooks",
        ));
    }
    let (id, cwd) = if agent == Agent::Agy {
        (
            object.get("conversationId").and_then(|v| v.as_str()),
            object
                .get("workspacePaths")
                .and_then(|v| v.as_array())
                .and_then(|v| v.first())
                .and_then(|v| v.as_str()),
        )
    } else {
        (
            object.get("session_id").and_then(|v| v.as_str()),
            object.get("cwd").and_then(|v| v.as_str()),
        )
    };
    let mut session = Session::new(
        terminal_key.into(),
        agent,
        id.ok_or_else(|| invalid("hook input has no exact conversation ID"))?,
        PathBuf::from(cwd.ok_or_else(|| invalid("hook input has no working directory"))?),
    )?;
    if agent == Agent::Pi {
        let file = object
            .get("session_file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| invalid("Pi hook has no persistent session file"))?;
        session.session_file = Some(PathBuf::from(file).canonicalize()?);
        session.validate_resume()?;
    }
    session.validate()?;
    Ok(session)
}

/// Only the UI-owning bridges may explicitly clear a persistent selection.
pub fn unselected(agent: Agent, payload: &serde_json::Value) -> Result<bool> {
    let Some(event) = payload.get("event") else {
        return Ok(false);
    };
    if !matches!(agent, Agent::Pi | Agent::Opencode) || event.as_str() != Some("unselected") {
        return Err(invalid("unsupported conversation selection event"));
    }
    if payload
        .get("owner_pid")
        .and_then(|v| v.as_u64())
        .is_none_or(|pid| pid <= 1 || pid > i32::MAX as u64)
    {
        return Err(invalid("selection event has no valid owning process"));
    }
    Ok(true)
}
