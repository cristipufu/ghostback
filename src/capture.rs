//! Agent-specific identity adapters. All joins are owned-process joins.
use crate::lifecycle::Capture;
use crate::native::Process;
use crate::sessions::{Agent, Session, Store};
use crate::{Result, files, invalid, native, process};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Clone)]
pub struct Roots {
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub grok: PathBuf,
}
impl Roots {
    pub fn from_home(home: &Path) -> Self {
        let root = |key: &str, default: &str| {
            std::env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(default))
        };
        Self {
            claude: root("CLAUDE_CONFIG_DIR", ".claude"),
            codex: root("CODEX_HOME", ".codex"),
            grok: root("GROK_HOME", ".grok"),
        }
    }
}

#[derive(Default)]
pub struct Cache {
    files: HashMap<String, (Instant, Vec<PathBuf>)>,
    logs: HashMap<(String, PathBuf), LogCursor>,
}

struct LogCursor {
    identity: (u64, u64),
    offset: u64,
    selection: Selection,
}
impl Cache {
    pub fn open_files(&mut self, owner: &Process) -> Result<Vec<PathBuf>> {
        let key = owner.key();
        if let Some((at, paths)) = self.files.get(&key)
            && at.elapsed() < Duration::from_secs(15)
        {
            return Ok(paths.clone());
        }
        if self.files.len() > 128 {
            self.files.clear();
        }
        let text = process::output(
            "/usr/sbin/lsof",
            &["-n", "-P", "-a", "-p", &owner.pid.to_string(), "-Fn"],
            Duration::from_secs(5),
        )?;
        let paths = text
            .lines()
            .filter_map(|line| {
                line.strip_prefix("n/")
                    .map(|value| PathBuf::from(format!("/{value}")))
            })
            .collect::<Vec<_>>();
        self.files.insert(key, (Instant::now(), paths.clone()));
        Ok(paths)
    }

    pub fn agy_log(&mut self, owner: &Process, path: &Path) -> Result<Selection> {
        use std::io::{Read, Seek, SeekFrom};
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        let mut file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        #[cfg(unix)]
        let identity = (metadata.dev(), metadata.ino());
        #[cfg(not(unix))]
        let identity = (0, 0);
        if self.logs.len() > 128 {
            self.logs.clear();
        }
        let cursor = self
            .logs
            .entry((owner.key(), path.into()))
            .or_insert(LogCursor {
                identity,
                offset: 0,
                selection: Selection::Unseen,
            });
        if cursor.identity != identity || metadata.len() < cursor.offset {
            *cursor = LogCursor {
                identity,
                offset: 0,
                selection: Selection::Unseen,
            };
        }
        let start = cursor
            .offset
            .max(metadata.len().saturating_sub(files::MAX_JSON));
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.take(files::MAX_JSON).read_to_end(&mut bytes)?;
        let Some(end) = bytes.iter().rposition(|b| *b == b'\n').map(|i| i + 1) else {
            if bytes.len() as u64 == files::MAX_JSON {
                return Err(invalid("agy log entry exceeds the read limit"));
            }
            return Ok(cursor.selection);
        };
        let skip = if start > cursor.offset {
            cursor.selection = Selection::Unseen;
            bytes
                .iter()
                .position(|b| *b == b'\n')
                .map_or(end, |i| i + 1)
        } else {
            0
        };
        let selection = parse_agy_selection(&String::from_utf8_lossy(&bytes[skip..end]))?;
        if selection != Selection::Unseen {
            cursor.selection = selection;
        }
        cursor.offset = start + end as u64;
        Ok(cursor.selection)
    }
}

pub fn registry_session(
    agent: Agent,
    input: &Value,
    owner: &Process,
) -> Result<Option<(Uuid, PathBuf)>> {
    match agent {
        Agent::Claude => {
            if input.get("pid").and_then(Value::as_u64) != Some(owner.pid as u64) {
                return Ok(None);
            }
            let started = input
                .get("startedAt")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("Claude registry has no process timestamp"))?;
            if started.saturating_add(2000) < owner.started_us / 1000 {
                return Ok(None);
            }
            entry(input, "sessionId")
        }
        Agent::Grok => {
            let entries = input
                .as_array()
                .ok_or_else(|| invalid("invalid Grok registry"))?;
            let matches: Vec<_> = entries
                .iter()
                .filter(|value| {
                    value.get("pid").and_then(Value::as_u64) == Some(owner.pid as u64)
                        && value.get("pid_start").and_then(Value::as_u64) == Some(owner.started_us)
                })
                .collect();
            match matches.as_slice() {
                [row] => entry(row, "session_id"),
                [] => Ok(None),
                _ => Err(invalid("ambiguous Grok process registry")),
            }
        }
        _ => Err(invalid("this agent does not use a JSON process registry")),
    }
}

fn entry(value: &Value, key: &str) -> Result<Option<(Uuid, PathBuf)>> {
    let id = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("registry has no conversation UUID"))?;
    let cwd = value
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("registry has no working directory"))?;
    Ok(Some((Uuid::parse_str(id)?, cwd.into())))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Selection {
    #[default]
    Unseen,
    Cleared,
    Selected(Uuid),
}

pub fn agy_selection(log: &str) -> Result<Option<Uuid>> {
    Ok(match parse_agy_selection(log)? {
        Selection::Selected(id) => Some(id),
        _ => None,
    })
}

pub fn parse_agy_selection(log: &str) -> Result<Selection> {
    let mut selected = Selection::Unseen;
    for line in log.lines() {
        let Some((prefix, message)) = line.split_once("] ") else {
            continue;
        };
        if !prefix.starts_with('I')
            || !prefix
                .get(1..5)
                .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit()))
            || !prefix.contains(".go:")
        {
            continue;
        }
        if let Some(tail) = message
            .strip_prefix("Streaming conversation ")
            .or_else(|| message.strip_prefix("Starting conversation update stream for "))
        {
            selected = Selection::Selected(Uuid::parse_str(
                tail.get(..36)
                    .ok_or_else(|| invalid("truncated agy conversation ID"))?,
            )?);
        } else if let Some(tail) = message.strip_prefix("Full redraw completed ")
            && let Some((_, id)) = tail.split_once(" for conversation ")
        {
            let id = id.trim_start();
            selected = if id.starts_with("(epoch") || id.trim().is_empty() {
                Selection::Cleared
            } else {
                Selection::Selected(Uuid::parse_str(
                    id.get(..36)
                        .ok_or_else(|| invalid("truncated agy redraw identity"))?,
                )?)
            };
        }
    }
    Ok(selected)
}

pub use crate::agents::replay_options;

pub fn resolve(
    store: &Store,
    roots: &Roots,
    owner: &Process,
    terminal: &str,
    cwd: &Path,
    cache: &mut Cache,
) -> Result<Option<Capture>> {
    let agent = owner
        .agent()
        .ok_or_else(|| invalid("not a supported interactive agent"))?;
    let shell = native::shell_ancestor(owner)?;
    let from_hook = store.bindings()?.into_iter().find(|b| {
        b.session.terminal_key == terminal && b.owner_key == owner.key() && b.phase == "active"
    });
    let mut allow_hook = true;
    let found = match agent {
        Agent::Pi | Agent::Opencode => {
            if let Some(b) = from_hook {
                if native::process(owner.pid)?.key() != owner.key() {
                    return Err(invalid("agent process changed during capture"));
                }
                return Ok(Some(Capture {
                    session: b.session,
                    shell_key: shell.key(),
                    owner_key: owner.key(),
                    executable: b.executable,
                    options: b.options,
                }));
            }
            return Ok(None);
        }
        Agent::Claude => read_optional(
            &roots
                .claude
                .join("sessions")
                .join(format!("{}.json", owner.pid)),
        )?
        .map(|value| registry_session(agent, &value, owner))
        .transpose()?
        .flatten(),
        Agent::Grok => read_optional(&roots.grok.join("active_sessions.json"))?
            .map(|value| registry_session(agent, &value, owner))
            .transpose()?
            .flatten(),
        Agent::Agy => {
            let paths = cache.open_files(owner)?;
            let custom = option_value(&owner.argv, "--log-file").map(|p| {
                if Path::new(p).is_absolute() {
                    PathBuf::from(p)
                } else {
                    cwd.join(p)
                }
            });
            let paths: HashSet<_> = paths
                .into_iter()
                .filter(|p| {
                    p.to_str().is_some_and(|s| {
                        s.contains("/antigravity-cli/log/cli-") && s.ends_with(".log")
                    }) || custom.as_ref() == Some(p)
                })
                .collect();
            let mut ids = HashSet::new();
            for path in paths {
                let selection = cache.agy_log(owner, &path)?;
                if selection != Selection::Unseen {
                    ids.insert(selection);
                }
            }
            if ids.len() > 1 {
                return Err(invalid("agy has conflicting process-owned logs"));
            }
            match ids.into_iter().next() {
                Some(Selection::Selected(id)) => Some((id, cwd.into())),
                Some(Selection::Cleared) => {
                    allow_hook = false;
                    None
                }
                _ => None,
            }
        }
        Agent::Codex => {
            if let Some(binding) = from_hook.as_ref() {
                Some((
                    Uuid::parse_str(binding.session.session_id.as_str())?,
                    binding.session.cwd.clone(),
                ))
            } else {
                let mut paths = cache.open_files(owner)?;
                // A dedicated app-server can own the rollout instead of the TUI.
                for child in child_pids(owner.pid)? {
                    if let Ok(info) = native::process(child)
                        && info.name == "codex"
                    {
                        paths.extend(cache.open_files(&info)?);
                    }
                }
                let ids: HashSet<_> = paths
                    .iter()
                    .filter(|p| p.starts_with(roots.codex.join("sessions")))
                    .filter_map(|p| {
                        p.file_stem()?
                            .to_str()?
                            .get(p.file_stem()?.to_str()?.len().checked_sub(36)?..)
                    })
                    .filter_map(|s| Uuid::parse_str(s).ok())
                    .collect();
                if ids.len() > 1 {
                    return Err(invalid(
                        "Codex has several loaded conversations; waiting for an exact lifecycle hook",
                    ));
                }
                ids.into_iter().next().map(|id| (id, cwd.into()))
            }
        }
    };
    let Some((id, directory)) = found.or_else(|| {
        from_hook.filter(|_| allow_hook).and_then(|b| {
            Uuid::parse_str(b.session.session_id.as_str())
                .ok()
                .map(|id| (id, b.session.cwd))
        })
    }) else {
        return Ok(None);
    };
    // Recheck process identity after reading metadata to defeat PID reuse.
    if native::process(owner.pid)?.key() != owner.key() {
        return Err(invalid("agent process changed during capture"));
    }
    Ok(Some(Capture {
        session: Session::new(terminal.into(), agent, &id.to_string(), directory)?,
        shell_key: shell.key(),
        owner_key: owner.key(),
        executable: owner.executable.clone(),
        options: replay_options(agent, owner.agent_argv(agent))?,
    }))
}

fn read_optional(path: &Path) -> Result<Option<Value>> {
    match files::read(path, files::MAX_JSON) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
fn option_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(i, arg)| {
        if arg == flag {
            args.get(i + 1).map(String::as_str)
        } else {
            arg.strip_prefix(&format!("{flag}="))
        }
    })
}

pub fn child_pids(pid: u32) -> Result<Vec<u32>> {
    let text = process::output("/bin/ps", &["-axo", "pid=,ppid="], Duration::from_secs(5))?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let child = parts.next()?.parse().ok()?;
            let parent: u32 = parts.next()?.parse().ok()?;
            (parent == pid).then_some(child)
        })
        .collect())
}
