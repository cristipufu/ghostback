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
}

impl Agent {
    pub fn name(self) -> &'static str {
        match self {
            Self::Agy => "agy",
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Grok => "grok",
        }
    }

    pub fn resume_flag(self) -> &'static str {
        match self {
            Self::Agy => "--conversation",
            Self::Codex => "resume",
            Self::Claude | Self::Grok => "--resume",
        }
    }

    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "agy" => Ok(Self::Agy),
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "grok" => Ok(Self::Grok),
            _ => Err(invalid(format!("unsupported agent: {name}"))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub terminal_key: String,
    pub agent: Agent,
    pub session_id: Uuid,
    pub cwd: PathBuf,
}

impl Session {
    pub fn new(terminal_key: String, agent: Agent, session_id: &str, cwd: PathBuf) -> Result<Self> {
        let session = Self {
            terminal_key: canonical_terminal_key(&terminal_key),
            agent,
            session_id: Uuid::parse_str(session_id)
                .map_err(|_| invalid("session ID must be an exact conversation UUID"))?,
            cwd,
        };
        session.validate()?;
        Ok(session)
    }

    pub fn validate(&self) -> Result<()> {
        if self.session_id.is_nil() {
            return Err(invalid("conversation UUID cannot be nil"));
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

    pub fn argv(&self) -> Vec<String> {
        vec![
            self.agent.name().into(),
            self.agent.resume_flag().into(),
            self.session_id.to_string(),
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
            self.argv().join(" ")
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
        if version > 2 || (!write && version == 0) {
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
                    recorded_at TEXT NOT NULL
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
                CREATE TABLE IF NOT EXISTS health (
                    key TEXT PRIMARY KEY, value TEXT NOT NULL
                );
                PRAGMA user_version=2;
                COMMIT;",
            )?;
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
        let id = Uuid::parse_str(session_id)?;
        if !self.path().try_exists()? {
            return Ok(false);
        }
        let mut db = self
            .connect(true)?
            .ok_or_else(|| invalid("state database unavailable"))?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let removed = tx.execute(
            "DELETE FROM sessions WHERE terminal_key=?1 AND session_id=?2",
            params![canonical_terminal_key(terminal_key), id.to_string()],
        )? > 0;
        if removed {
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
        "INSERT INTO sessions VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(terminal_key) DO UPDATE SET
                agent=excluded.agent, session_id=excluded.session_id,
                cwd=excluded.cwd, recorded_at=excluded.recorded_at",
        params![
            canonical_terminal_key(&session.terminal_key),
            session.agent.name(),
            session.session_id.to_string(),
            session.cwd.to_str(),
            timestamp
        ],
    )?;
    Ok(())
}

pub(crate) fn list_sessions(db: &Connection) -> Result<Vec<Session>> {
    let mut statement = db.prepare(
        "SELECT terminal_key, agent, session_id, cwd FROM sessions ORDER BY terminal_key",
    )?;
    let mut rows = statement.query([])?;
    let mut sessions = Vec::new();
    while let Some(row) = rows.next()? {
        let agent: String = row.get(1)?;
        let id: String = row.get(2)?;
        sessions.push(Session::new(
            row.get(0)?,
            Agent::parse(&agent)?,
            &id,
            PathBuf::from(row.get::<_, String>(3)?),
        )?);
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
    Session::new(
        terminal_key.into(),
        agent,
        id.ok_or_else(|| invalid("hook input has no exact conversation UUID"))?,
        PathBuf::from(cwd.ok_or_else(|| invalid("hook input has no working directory"))?),
    )
}
