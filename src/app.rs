//! Application workflows. Native effects stay outside the lifecycle state machine.
use crate::capture::{self, Cache, Roots};
use crate::lifecycle::{Capture, Decision};
use crate::sessions::{Agent, Store, hook_session};
use crate::{Result, invalid, macos, native, process};
use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub fn executable(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .filter(|p| p.is_absolute())
            .map(|p| p.join(name))
            .find(|p| is_executable(p))
    })
}
pub fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn agent_executable(agent: Agent, owner: &native::Process) -> PathBuf {
    let actual = owner.executable.canonicalize().ok();
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(agent.name()))
                .find(|candidate| {
                    actual.is_some()
                        && is_executable(candidate)
                        && candidate.canonicalize().ok() == actual
                })
        })
        .unwrap_or_else(|| owner.executable.clone())
}
pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| invalid("HOME is missing"))
}

pub fn register_hook(store: &Store, agent: Agent, payload: serde_json::Value) -> Result<()> {
    let unselected = crate::sessions::unselected(agent, &payload)?;
    let tick = if matches!(agent, Agent::Pi | Agent::Opencode) {
        Some(
            payload
                .get("observed_ns")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid("bridge observation has no monotonic timestamp"))?
                .parse::<u64>()?,
        )
    } else {
        None
    };
    let session = if unselected {
        None
    } else {
        Some(hook_session(
            agent,
            payload.clone(),
            "pending-native-identity",
        )?)
    };
    let mut current = native::process(std::process::id())?;
    let mut owner = None;
    for _ in 0..32 {
        current = native::process(current.parent)?;
        if current.agent() == Some(agent)
            && crate::agents::interactive(agent, current.agent_argv(agent))
        {
            owner = Some(current.clone());
            break;
        }
    }
    let owner = owner.ok_or_else(|| invalid("hook has no owning interactive agent process"))?;
    if matches!(agent, Agent::Pi | Agent::Opencode)
        && payload.get("owner_pid").and_then(|v| v.as_u64()) != Some(u64::from(owner.pid))
    {
        return Err(invalid(
            "bridge is not running in its owning interactive agent",
        ));
    }
    let shell = native::shell_ancestor(&owner)?;
    let hint = std::env::var("GHOSTBACK_TERMINAL_KEY").ok();
    let host = std::env::var("GHOSTBACK_HOST_KEY").ok();
    let key = if let (Some(hint), Some(host)) = (hint, host) {
        let mut ancestor = owner.clone();
        let mut valid = false;
        for _ in 0..32 {
            if ancestor.key() == host {
                valid = true;
                break;
            }
            if ancestor.parent <= 1 {
                break;
            }
            ancestor = native::process(ancestor.parent)?;
        }
        if !valid {
            return Err(invalid("hook host identity is stale"));
        }
        uuid::Uuid::parse_str(&hint)?.to_string()
    } else {
        let key = macos::terminal_key_for_process(owner.pid)?;
        if native::process(owner.pid)?.key() != owner.key() {
            return Err(invalid("hook owner changed during terminal identification"));
        }
        key
    };
    let Some(mut session) = session else {
        store.clear_selection(
            &key,
            &owner.key(),
            tick.ok_or_else(|| invalid("missing bridge timestamp"))?,
        )?;
        return Ok(());
    };
    session.terminal_key = key;
    let (binary, options) = if agent == Agent::Pi {
        // Pi changes process.title, erasing kernel argv. Its in-process argv remains intact.
        let args: Vec<String> = serde_json::from_value(
            payload
                .get("launch_argv")
                .cloned()
                .ok_or_else(|| invalid("Pi bridge has no launch arguments"))?,
        )?;
        if args.len() > 256 || args.iter().any(|s| s.len() > 8192 || s.contains('\0')) {
            return Err(invalid("Pi launch arguments exceed capture limits"));
        }
        let script = args
            .first()
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && is_executable(p))
            .ok_or_else(|| invalid("Pi CLI script is unavailable"))?;
        let binary = executable("pi")
            .filter(|p| p.canonicalize().ok() == script.canonicalize().ok())
            .unwrap_or(script);
        (binary, capture::replay_options(agent, &args)?)
    } else {
        (
            agent_executable(agent, &owner),
            capture::replay_options(agent, owner.agent_argv(agent))?,
        )
    };
    store.capture_observed(
        &Capture {
            session,
            shell_key: shell.key(),
            owner_key: owner.key(),
            executable: binary,
            options,
        },
        tick,
    )
}

#[derive(Default)]
pub struct WatchState {
    cache: Cache,
    generation: Option<String>,
    previous: HashSet<String>,
    missing: std::collections::HashMap<String, u8>,
}

fn ghostty_generation() -> Result<Option<String>> {
    let text = process::output("/bin/ps", &["-axo", "pid=,comm="], Duration::from_secs(5))?;
    let mut found = Vec::new();
    for line in text.lines() {
        let mut fields = line.trim().splitn(2, char::is_whitespace);
        let pid = fields.next().and_then(|s| s.parse::<u32>().ok());
        let name = fields.next().unwrap_or("").trim();
        if name.ends_with("/Ghostty.app/Contents/MacOS/ghostty")
            && let Some(pid) = pid
        {
            found.push(native::process(pid)?.key());
        }
    }
    Ok(if found.len() == 1 { found.pop() } else { None })
}

impl WatchState {
    pub fn closed_terminals(
        &mut self,
        generation: Option<String>,
        current: HashSet<String>,
    ) -> Vec<String> {
        let same_generation = generation.is_some() && generation == self.generation;
        if !same_generation {
            self.previous.clear();
            self.missing.clear();
        }
        self.generation = generation;
        let mut closed = Vec::new();
        if same_generation {
            for key in self.previous.difference(&current) {
                let count = self.missing.entry(key.clone()).or_default();
                *count = count.saturating_add(1);
                if *count >= 2 {
                    closed.push(key.clone());
                }
            }
        }
        for key in &closed {
            self.previous.remove(key);
            self.missing.remove(key);
        }
        for key in &current {
            self.missing.remove(key);
        }
        self.previous.extend(current);
        closed
    }
}

pub fn scan(store: &Store, roots: &Roots, state: &mut WatchState) -> Result<usize> {
    let terminals = macos::inventory()?;
    let generation = ghostty_generation()?;
    let current = terminals
        .iter()
        .map(|t| t.terminal_key.to_string())
        .collect();
    for key in state.closed_terminals(generation, current) {
        store.retire(&key, "Terminal was deliberately closed.")?;
    }
    let mut captured = 0;
    for terminal in terminals {
        let key = terminal.terminal_key.to_string();
        state.missing.remove(&key);
        let mut owner = match native::process(terminal.pid) {
            Ok(process) => process,
            Err(_) => continue,
        };
        for _ in 0..4 {
            if owner.name != "ghostback" && !owner.is_shell() {
                break;
            }
            let children = capture::child_pids(owner.pid)?;
            let matches: Vec<_> = children
                .into_iter()
                .filter_map(|pid| native::process(pid).ok())
                .filter(|p| p.agent().is_some() || p.name == "ghostback")
                .collect();
            if matches.len() != 1 {
                break;
            }
            owner = matches
                .into_iter()
                .next()
                .ok_or_else(|| invalid("agent disappeared"))?;
        }
        if owner.agent().is_none() {
            continue;
        }
        if owner
            .agent()
            .is_some_and(|agent| !crate::agents::interactive(agent, owner.agent_argv(agent)))
        {
            continue;
        }
        if matches!(owner.agent(), Some(Agent::Pi | Agent::Opencode)) {
            // Their UI bridge is authoritative. Rewriting a snapshot here could undo a switch.
            let binding = store
                .bindings()?
                .into_iter()
                .find(|b| b.session.terminal_key == key);
            if binding
                .as_ref()
                .is_some_and(|b| b.owner_key == owner.key() && b.phase == "active")
            {
                store.health(&format!("terminal.{key}"), "captured")?;
                captured += 1;
            } else {
                let reason = "Exact UI selection is unavailable. Restart the agent after ghostback setup; check plugins are enabled.";
                if let Some(binding) = binding {
                    store.attention_if_unchanged(
                        &key,
                        &binding.owner_key,
                        &binding.phase,
                        reason,
                    )?;
                }
                store.health(&format!("terminal.{key}"), reason)?;
            }
            continue;
        }
        let outcome = capture::resolve(store, roots, &owner, &key, &terminal.cwd, &mut state.cache);
        match outcome {
            Ok(Some(mut observed)) => {
                observed.executable = agent_executable(observed.session.agent, &owner);
                store.capture(&observed)?;
                store.health(&format!("terminal.{key}"), "captured")?;
                captured += 1;
            }
            Ok(None) => {
                let reason = "Exact conversation identity is unavailable. Start the agent after setup, or inspect ghostback status.";
                store.attention(&key, reason)?;
                store.health(&format!("terminal.{key}"), reason)?;
            }
            Err(error) => {
                store.attention(&key, &error.to_string())?;
                store.health(&format!("terminal.{key}"), &error.to_string())?;
            }
        }
    }
    store.health("watcher", "running")?;
    Ok(captured)
}

pub fn watch(store: &Store, once: bool) -> Result<()> {
    let _lock = crate::files::Lock::acquire(&store.directory.join("watch.lock"))?;
    if !once {
        store.health(
            "watcher.process",
            &native::process(std::process::id())?.key(),
        )?;
    }
    let roots = Roots::from_home(&home()?);
    let mut state = WatchState::default();
    let current = std::env::current_exe()?;
    loop {
        #[cfg(unix)]
        if !once {
            use std::os::unix::process::CommandExt;
            match crate::install::homebrew_upgrade(&store.directory, &current) {
                Ok(Some(binary)) => {
                    let error = Command::new(binary)
                        .arg("--state-dir")
                        .arg(&store.directory)
                        .arg("watch")
                        .exec();
                    store.health("watcher.upgrade", &error.to_string())?;
                }
                Err(error) => store.health("watcher.upgrade", &error.to_string())?,
                Ok(None) => {}
            }
        }
        if let Err(error) = scan(store, &roots, &mut state) {
            store.health("watcher", &error.to_string())?;
            if once {
                return Err(error);
            }
        }
        if once {
            store.health("watcher", "capture completed")?;
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

pub fn shell_ready(store: &Store, shell_pid: u32) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(invalid("shell-ready requires an interactive terminal"));
    }
    let me = native::process(std::process::id())?;
    let shell = native::process(shell_pid)?;
    if me.parent != shell_pid || !shell.is_shell() {
        return Err(invalid(
            "shell-ready must be invoked directly by its owning shell",
        ));
    }
    let key = macos::current_terminal_key()?;
    let Some(binding) = store
        .bindings()?
        .into_iter()
        .find(|b| b.session.terminal_key == key)
    else {
        return Ok(());
    };
    if native::alive(&binding.owner_key) {
        return Ok(());
    }
    if binding.shell_key != shell.key() && native::alive(&binding.shell_key) {
        return Ok(());
    }
    let decision = store.prepare(&key, &shell.key())?;
    if decision != Decision::Launch {
        return Ok(());
    }
    let binding = store
        .bindings()?
        .into_iter()
        .find(|b| {
            b.session.terminal_key == key && b.phase == "claim" && b.claim_shell == shell.key()
        })
        .ok_or_else(|| invalid("restore claim changed before launch"))?;
    if !binding.session.cwd.is_dir() {
        store.attention(
            &key,
            "Working directory is missing. Restore it, then run ghostback retry.",
        )?;
        return Ok(());
    }
    if let Err(error) = binding.session.validate_resume() {
        store.attention(&key, &error.to_string())?;
        return Ok(());
    }
    if !is_executable(&binding.executable) {
        store.attention(&key,"Agent executable moved or is unavailable. Run ghostback setup to refresh its path, then retry.")?;
        return Ok(());
    }
    crate::agents::validate_options(binding.session.agent, &binding.options)?;
    let _ = writeln!(
        io::stderr().lock(),
        "Ghostback · resuming {}",
        binding.session.agent.name()
    );
    let host = me.key();
    let mut command = Command::new(&binding.executable);
    let mut options = binding.options.clone();
    if binding.session.agent == Agent::Codex && !options.iter().any(|s| s == "--no-daemon") {
        options.push("--no-daemon".into());
    }
    command
        .args(&options)
        .arg(binding.session.agent.resume_flag())
        .arg(
            binding
                .session
                .session_file
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| binding.session.session_id.to_string()),
        )
        .current_dir(&binding.session.cwd)
        .env("GHOSTBACK_TERMINAL_KEY", &key)
        .env("GHOSTBACK_HOST_KEY", host);
    let result = command.status();
    match result {
        Ok(status) if status.success() => {
            let acknowledged = store.bindings()?.into_iter().any(|b| {
                b.session.terminal_key == key
                    && b.phase == "active"
                    && b.shell_key == shell.key()
                    && !native::alive(&b.owner_key)
            });
            if acknowledged {
                store.retire(&key, "Restored agent exited normally.")?;
            } else {
                store.attention(&key,"Agent exited before its exact conversation could be acknowledged. Inspect the session, then retry.")?;
            }
        }
        Ok(status) => {
            store.attention(
                &key,
                &format!("Agent exited with {status}. Check sign-in and ghostback retry."),
            )?;
        }
        Err(error) => {
            store.attention(&key, &format!("Could not launch agent: {error}"))?;
        }
    }
    Ok(())
}

pub fn run_codex(store: &Store, binary: &Path, args: &[String]) -> Result<i32> {
    if !binary.is_absolute() || !is_executable(binary) {
        return Err(invalid(
            "Codex executable is not an absolute executable file",
        ));
    }
    let shim = store.directory.join("shims/codex");
    if shim.exists() && shim.canonicalize()? == binary.canonicalize()? {
        return Err(invalid(
            "Codex shim cannot launch itself; rerun setup with the original Codex on PATH",
        ));
    }
    let passthrough = args
        .iter()
        .any(|s| matches!(s.as_str(), "--help" | "-h" | "--version" | "-V"))
        || args.first().is_some_and(|s| {
            matches!(
                s.as_str(),
                "exec"
                    | "doctor"
                    | "app-server"
                    | "login"
                    | "logout"
                    | "update"
                    | "mcp"
                    | "completion"
            )
        });
    let mut command = Command::new(binary);
    if !passthrough {
        if !args.iter().any(|s| s == "--no-daemon") {
            command.arg("--no-daemon");
        }
        match macos::current_terminal_key() {
            Ok(key) => {
                command.env("GHOSTBACK_TERMINAL_KEY", key).env(
                    "GHOSTBACK_HOST_KEY",
                    native::process(std::process::id())?.key(),
                );
            }
            Err(error) => {
                store.health("codex.capture", &error.to_string())?;
            }
        }
    }
    let status = command.args(args).status()?;
    Ok(status.code().unwrap_or(130))
}
