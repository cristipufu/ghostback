use clap::{Parser, Subcommand};
use ghostback::sessions::{Agent, Session, Store, default_state_dir, hook_session};
use ghostback::{Error, Result, app, herd, install, invalid, macos, process, ui};
use serde_json::json;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_HOOK_BYTES: u64 = 1_048_576;

#[derive(Parser)]
#[command(
    version,
    about = "Recover exact coding-agent conversations in native Ghostty terminals."
)]
struct Cli {
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install automatic capture and recovery for this macOS account.
    Setup {
        #[arg(long)]
        dry_run: bool,
        #[arg(long, hide = true)]
        no_launch: bool,
    },
    /// Remove only Ghostback-owned integrations. Keep saved sessions.
    Uninstall {
        #[arg(long, hide = true)]
        no_launch: bool,
    },
    /// Inspect captured sessions, recovery outcomes, and helper health.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Capture once, or keep the background watcher running.
    Watch {
        #[arg(long)]
        once: bool,
    },
    /// Arm failed sessions for retry at their next shell prompt.
    #[command(alias = "restore")]
    Retry {
        #[arg(long)]
        terminal: Option<String>,
    },
    #[command(hide = true)]
    ShellReady {
        #[arg(long)]
        shell_pid: u32,
    },
    #[command(hide = true)]
    Run {
        #[arg(long)]
        binary: PathBuf,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Inspect installed tools without controlling Ghostty.
    Doctor,
    /// Read native Ghostty terminal identities on macOS.
    Inventory,
    /// Record an exact conversation for a terminal.
    Record {
        #[arg(long, value_enum)]
        agent: Agent,
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        session_file: Option<PathBuf>,
        #[arg(long)]
        terminal_key: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Record a native lifecycle hook supplied as JSON on stdin.
    Hook {
        #[arg(value_enum)]
        agent: Agent,
        /// Return native capture failures to the caller (for diagnostics).
        #[arg(long, hide = true, conflicts_with = "quiet")]
        strict: bool,
        /// Keep optional hook errors in Ghostback diagnostics.
        #[arg(long, hide = true)]
        quiet: bool,
    },
    /// Preview exact recovery commands. Does not execute them.
    Plan {
        #[arg(long)]
        json: bool,
    },
    /// Remove a specific conversation binding; late exits cannot erase a new one.
    Forget {
        #[arg(long)]
        terminal_key: String,
        #[arg(long)]
        session_id: String,
    },
    /// Interactive Herdr-style fleet board, table, and sidebar for Ghostty terminals.
    #[command(visible_aliases = ["board", "herd", "sidebar"])]
    Ui {
        /// Start in compact vertical sidebar mode.
        #[arg(long)]
        sidebar: bool,
        /// Split the current Ghostty terminal on the left and dock the sidebar UI.
        #[arg(long)]
        dock: bool,
        /// Render a single snapshot frame to stdout and exit.
        #[arg(long)]
        once: bool,
        /// Populate an in-memory 8-agent demo fleet for previewing the UI.
        #[arg(long)]
        demo: bool,
    },
    /// Spawn a child/reviewer agent in another harness and coordinate until done.
    #[command(visible_aliases = ["spawn-child", "pair"])]
    Delegate {
        /// Target child agent harness to spawn (codex, claude, agy, grok, opencode, pi).
        #[arg(long, value_enum)]
        to: Agent,
        /// Role for the child agent (reviewer, worker, advisor).
        #[arg(long, value_enum, default_value_t = herd::TaskRole::Reviewer)]
        role: herd::TaskRole,
        /// Optional model name or slug for the child agent (e.g. sol-6.1, gpt-5.1-codex, claude-opus-4-6).
        #[arg(long, short = 'm')]
        model: Option<String>,
        /// Optional reasoning/thinking effort level (minimal, low, medium, high, xhigh, max).
        #[arg(long, short = 'e')]
        effort: Option<String>,
        /// Optional extra CLI flags to forward to the child harness.
        #[arg(long = "arg")]
        extra_args: Vec<String>,
        /// Task brief or review prompt for the child agent.
        #[arg(long)]
        prompt: Option<String>,
        /// Optional parent terminal UUID (auto-detected from Ghostty TTY when omitted).
        #[arg(long)]
        parent: Option<String>,
        /// Optional working directory (defaults to current directory).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Block until the child agent sends its reply via `ghostback reply`.
        #[arg(long)]
        wait: bool,
        /// Maximum seconds to wait per CLI call when `--wait` is enabled (defaults to 90s to stay under 120s Bash tool timeouts).
        #[arg(long, default_value_t = 90)]
        timeout_secs: u64,
        /// Skip spawning a native Ghostty surface (for headless/scripted testing).
        #[arg(long, hide = true)]
        no_spawn: bool,
    },
    /// Run the built-in stdio MCP server exposing Ghostback Herd cross-harness orchestration tools.
    Mcp,
    /// Send a follow-up revision or message from the parent agent to a child agent.
    Ask {
        /// Task ID (auto-detected from current parent terminal if omitted).
        #[arg(long)]
        task: Option<String>,
        /// Block until the child agent responds via `ghostback reply`.
        #[arg(long)]
        wait: bool,
        /// Maximum seconds to wait per CLI call when `--wait` is enabled.
        #[arg(long, default_value_t = 90)]
        timeout_secs: u64,
        /// Message or revision summary to send to the child agent.
        message: String,
    },
    /// Send a review verdict or task update from a child agent back to its parent.
    Reply {
        /// Task ID (auto-detected from GHOSTBACK_TASK_ID or current child terminal if omitted).
        #[arg(long)]
        task: Option<String>,
        /// Verdict / status of this turn (changes-requested, approved, done, comment).
        #[arg(long, value_enum, default_value_t = herd::ReplyVerdict::ChangesRequested)]
        status: herd::ReplyVerdict,
        /// Block until the parent agent sends the next round via `ghostback ask`.
        #[arg(long)]
        wait: bool,
        /// Maximum seconds to wait per CLI call when `--wait` is enabled.
        #[arg(long, default_value_t = 90)]
        timeout_secs: u64,
        /// Detailed findings, review feedback, or sign-off message.
        message: String,
    },
    /// Continue waiting for a peer agent's response on an active Herd task without sending a duplicate message.
    Wait {
        /// Task ID (auto-detected from GHOSTBACK_TASK_ID or current terminal if omitted).
        #[arg(long)]
        task: Option<String>,
        /// Which peer to wait for (`parent` or `child`; auto-detected when omitted).
        #[arg(long = "for")]
        r#for: Option<String>,
        /// Maximum seconds to wait before returning a continuation prompt.
        #[arg(long, default_value_t = 90)]
        timeout_secs: u64,
    },
    /// Inspect cross-harness parent-child tasks and conversation rounds.
    #[command(visible_alias = "messages")]
    Tasks {
        /// Optional task ID to inspect full message history.
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

fn find_program(name: &str) -> Option<PathBuf> {
    app::executable(name)
}

fn doctor(state_dir: &Path) -> serde_json::Value {
    let ghostty = find_program("ghostty").or_else(|| {
        let path = PathBuf::from("/Applications/Ghostty.app/Contents/MacOS/ghostty");
        path.is_file().then_some(path)
    });
    let version = ghostty.as_ref().map(|path| {
        let path = path
            .to_str()
            .ok_or_else(|| invalid("Ghostty executable path is not valid UTF-8"))?;
        process::output(path, &["+version"], Duration::from_secs(10))
    });
    let (version, error) = match version {
        Some(Ok(text)) => (text.lines().next().map(str::to_owned), None),
        Some(Err(error)) => (None, Some(error.to_string())),
        None => (None, Some("Ghostty executable not found".into())),
    };
    let store = Store::new(state_dir.to_owned()).ok();
    let helper = store
        .as_ref()
        .and_then(|s| s.health_status().ok())
        .and_then(|health| health.get("watcher.process").cloned())
        .is_some_and(|key| ghostback::native::alive(&key));
    json!({
        "installation": {"configured":state_dir.join("install.json").is_file(), "pending_setup":state_dir.join("setup-pending.json").is_file(), "helper_alive":helper},
        "ghostback": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "ghostty": {"path": display_path(ghostty), "version": version, "error": error},
        "agents": {"agy": display_path(find_program("agy")), "codex": display_path(find_program("codex")), "claude": display_path(find_program("claude")), "grok": display_path(find_program("grok")), "pi": display_path(find_program("pi")), "opencode": display_path(find_program("opencode"))},
        "state_directory": state_dir.to_string_lossy(),
        "automatic_restore": "available after ghostback setup; runs at restored shell prompts",
        "terminal_integration_target": "macOS, Ghostty 1.4+"
    })
}

fn display_path(path: Option<PathBuf>) -> Option<String> {
    path.map(|value| value.to_string_lossy().into_owned())
}

fn read_hook(store: &Store, agent: Agent, strict: bool) -> Result<()> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(MAX_HOOK_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_HOOK_BYTES {
        return Err(invalid("hook payload exceeds the 1 MiB limit"));
    }
    let payload = serde_json::from_slice(&bytes)?;
    // Reject malformed input before native automation or diagnostic writes.
    if !ghostback::sessions::unselected(agent, &payload)? {
        hook_session(agent, payload.clone(), "unbound")?;
    }
    if std::env::var_os("GHOSTTY_SURFACE_ID").is_some()
        || std::env::var_os("GHOSTBACK_HOST_KEY").is_some()
    {
        let result = app::register_hook(store, agent, payload);
        let diagnostic = match result {
            Ok(()) => "captured".to_owned(),
            Err(error) if strict || matches!(agent, Agent::Pi | Agent::Opencode) => {
                return Err(error);
            }
            Err(error) => format!("Capture skipped: {error}"),
        };
        // An optional native hook must never disrupt the user's agent prompt.
        // A diagnostic write failure must not turn into a second hook failure.
        let _ = store.health(&format!("hook.{}", agent.name()), &diagnostic);
    }
    Ok(())
}

fn run(cli: Cli, out: &mut impl Write) -> Result<i32> {
    let state_dir = match cli.state_dir {
        Some(path) => path,
        None => default_state_dir()?,
    };
    let store = Store::new(state_dir)?;
    match cli.command {
        Commands::Setup { dry_run, no_launch } => {
            let paths = install::setup(&install::Options {
                home: app::home()?,
                state: store.directory.clone(),
                source: std::env::current_exe()?,
                dry_run,
                launch: !no_launch,
            })?;
            for path in paths {
                writeln!(
                    out,
                    "{} {}",
                    if dry_run {
                        "Would configure"
                    } else {
                        "Configured"
                    },
                    path.display()
                )?;
            }
            if !dry_run {
                writeln!(
                    out,
                    "\nGhostback is installed. Open a new Ghostty tab to load shell integration.\nAllow macOS Automation access when prompted. Codex may ask you to trust its new hook once.\nRun ghostback status to inspect capture. Existing agents without terminal-bound metadata must be started once after setup."
                )?;
            }
        }
        Commands::Uninstall { no_launch } => {
            for path in install::uninstall(&store.directory, !no_launch)? {
                writeln!(out, "Removed integration: {}", path.display())?;
            }
            writeln!(
                out,
                "Saved sessions are retained. Open a new shell to discard already-loaded functions."
            )?;
        }
        Commands::Status { json: as_json } => {
            let bindings = store.bindings()?;
            let mut health = store.health_status()?;
            if let Some(key) = health.get("watcher.process") {
                let alive = ghostback::native::alive(key);
                health.insert("watcher.alive".into(), alive.to_string());
            }
            if as_json {
                writeln!(
                    out,
                    "{}",
                    serde_json::to_string_pretty(&json!({"sessions":bindings,"health":health}))?
                )?;
            } else {
                if bindings.is_empty() {
                    writeln!(
                        out,
                        "No captured sessions yet. Run ghostback setup and open a new Ghostty tab."
                    )?;
                }
                for binding in bindings {
                    writeln!(
                        out,
                        "{:<10} {:<6} {}\n  terminal: {}\n  {}\n  {}",
                        binding.phase,
                        binding.session.agent.name(),
                        binding.session.session_id,
                        binding.session.terminal_key,
                        binding.session.cwd.display(),
                        binding.reason
                    )?;
                }
                for (key, value) in health {
                    if value != "captured" {
                        writeln!(out, "{key}: {value}")?;
                    }
                }
            }
        }
        Commands::Watch { once } => app::watch(&store, once)?,
        Commands::Retry { terminal } => {
            let count = store.retry(terminal.as_deref())?;
            writeln!(
                out,
                "Armed {count} session(s). Press Enter in each affected terminal; recovery never types into a busy tab."
            )?;
        }
        Commands::ShellReady { shell_pid } => {
            if let Err(error) = app::shell_ready(&store, shell_pid) {
                store.health("restore", &error.to_string())?;
            }
        }
        Commands::Run { args, binary } => return app::run_codex(&store, &binary, &args),
        Commands::Doctor => writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&doctor(&store.directory))?
        )?,
        Commands::Inventory => writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&macos::inventory()?)?
        )?,
        Commands::Record {
            agent,
            session_id,
            session_file,
            terminal_key,
            cwd,
        } => {
            let cwd = cwd.unwrap_or(std::env::current_dir()?);
            let cwd = cwd.canonicalize()?;
            if !cwd.is_dir() {
                return Err(invalid("working directory is not a directory"));
            }
            let mut session = Session::new(terminal_key, agent, &session_id, cwd)?;
            session.session_file = session_file.map(|p| p.canonicalize()).transpose()?;
            session.validate_resume()?;
            store.record(&session)?;
            writeln!(
                out,
                "Recorded {} {} for {}.",
                agent.name(),
                session.session_id,
                session.terminal_key
            )?;
        }
        Commands::Hook {
            agent,
            strict,
            quiet,
        } => {
            if let Err(error) = read_hook(&store, agent, strict) {
                if !quiet {
                    return Err(error);
                }
                let _ = store.health(
                    &format!("hook.{}", agent.name()),
                    &format!("Capture skipped: {error}"),
                );
            }
        }
        Commands::Plan { json: as_json } => {
            let sessions = store.list()?;
            if as_json {
                let rows: Vec<_> = sessions.iter().map(|s| json!({
                    "terminal_key": s.terminal_key, "agent": s.agent, "session_id": s.session_id,
                    "cwd": s.cwd, "session_file": s.session_file, "argv": s.argv(), "cwd_exists": s.cwd.is_dir()
                })).collect();
                writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
            } else if sessions.is_empty() {
                writeln!(
                    out,
                    "No recorded sessions. Run ghostback setup, then check ghostback status."
                )?;
            } else {
                for session in sessions {
                    writeln!(out, "# {}\n{}", session.terminal_key, session.command()?)?;
                    if !session.cwd.is_dir() {
                        writeln!(
                            io::stderr().lock(),
                            "ghostback: directory is missing: {}",
                            session.cwd.display()
                        )?;
                    }
                }
            }
        }
        Commands::Forget {
            terminal_key,
            session_id,
        } => {
            let removed = store.forget(&terminal_key, &session_id)?;
            writeln!(
                out,
                "{}",
                if removed {
                    "Forgot recorded conversation."
                } else {
                    "No matching conversation; nothing changed."
                }
            )?;
        }
        Commands::Ui {
            sidebar,
            dock,
            once,
            demo,
        } => {
            ui::run(&store, sidebar, dock, once, demo, out)?;
        }
        Commands::Delegate {
            to,
            role,
            model,
            effort,
            extra_args,
            prompt,
            parent,
            cwd,
            wait,
            timeout_secs,
            no_spawn,
        } => {
            let cwd = cwd.unwrap_or(std::env::current_dir()?);
            let cwd = cwd.canonicalize().unwrap_or(cwd);
            let exe = std::env::current_exe()?;
            let raw_prompt = prompt.as_deref().unwrap_or("");
            let (task, seq, _) = herd::delegate_task(
                &store,
                &herd::DelegateSpec {
                    exe: &exe,
                    parent_key: parent.as_deref(),
                    parent_agent: None,
                    target_uuid: None,
                    child_agent: to,
                    role,
                    raw_prompt,
                    options: herd::AgentLaunchOptions {
                        model,
                        effort,
                        extra_args,
                    },
                    cwd: &cwd,
                    no_spawn,
                    sidebar_cmd: None,
                },
            )?;
            if wait && !no_spawn {
                let text = herd::wait_or_timeout_for_parent(
                    &store,
                    &exe,
                    task,
                    seq,
                    Duration::from_secs(timeout_secs.max(1)),
                )?;
                write!(out, "{text}")?;
            } else {
                writeln!(
                    out,
                    "Spawned {} ({}) for task {} in terminal {}.\nWait for reply with:\n  ghostback wait --task {} --for child",
                    to.name(),
                    role.label(),
                    task.task_id,
                    task.child_terminal_key,
                    task.task_id
                )?;
            }
        }
        Commands::Mcp => {
            herd::run_mcp_server(&store, io::stdin().lock(), out)?;
        }
        Commands::Ask {
            task,
            wait,
            timeout_secs,
            message,
        } => {
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
            let my_key = herd::resolve_caller_terminal_key(&store);
            let task_id = herd::resolve_task_id(&store, task.as_deref(), my_key.as_deref(), false)?;
            let (updated_task, seq) = herd::record_parent_message(&store, &task_id, &message)?;
            if wait {
                let text = herd::wait_or_timeout_for_parent(
                    &store,
                    &exe,
                    updated_task,
                    seq,
                    Duration::from_secs(timeout_secs.max(1)),
                )?;
                write!(out, "{text}")?;
            } else {
                writeln!(
                    out,
                    "Sent round {} update to {} on task {}.",
                    updated_task.round,
                    updated_task.child_agent.name(),
                    updated_task.task_id
                )?;
            }
        }
        Commands::Reply {
            task,
            status,
            wait,
            timeout_secs,
            message,
        } => {
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
            let my_key = herd::resolve_caller_terminal_key(&store);
            let task_id = herd::resolve_task_id(&store, task.as_deref(), my_key.as_deref(), true)?;
            let (updated_task, seq) = herd::record_child_reply(&store, &task_id, status, &message)?;
            if wait && !status.is_terminal() {
                let text = herd::wait_or_timeout_for_child(
                    &store,
                    &exe,
                    updated_task,
                    seq,
                    Duration::from_secs(timeout_secs.max(1)),
                )?;
                write!(out, "{text}")?;
            } else {
                writeln!(
                    out,
                    "Recorded {} reply (round {}) for task {}.",
                    status.as_str(),
                    updated_task.round,
                    updated_task.task_id
                )?;
            }
        }
        Commands::Wait {
            task,
            r#for,
            timeout_secs,
        } => {
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
            let expected = match r#for
                .as_deref()
                .map(|s| s.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("parent") => "parent",
                Some("child") => "child",
                _ => {
                    if std::env::var_os("GHOSTBACK_TASK_ID").is_some() {
                        "parent"
                    } else {
                        "child"
                    }
                }
            };
            let is_child = expected == "parent";
            let my_key = herd::resolve_caller_terminal_key(&store);
            let task_id =
                herd::resolve_task_id(&store, task.as_deref(), my_key.as_deref(), is_child)?;
            let text = herd::wait_for_peer_formatted(
                &store,
                &exe,
                &task_id,
                expected,
                Duration::from_secs(timeout_secs.max(1)),
            )?;
            write!(out, "{text}")?;
        }
        Commands::Tasks {
            task,
            json: as_json,
        } => {
            if let Some(task_id) = task {
                let (t, msgs) = herd::get_task(&store, &task_id)?
                    .ok_or_else(|| invalid(format!("unknown herd task: {task_id}")))?;
                if as_json {
                    writeln!(
                        out,
                        "{}",
                        serde_json::to_string_pretty(&t.to_json(Some(&msgs)))?
                    )?;
                } else {
                    writeln!(
                        out,
                        "Task {} · {} ⇄ {} ({}) · Round {} · {}",
                        t.task_id,
                        t.parent_agent.map(Agent::name).unwrap_or("parent"),
                        t.child_agent.name(),
                        t.role.label(),
                        t.round,
                        t.status.badge()
                    )?;
                    for m in msgs {
                        let v = if m.verdict.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", m.verdict)
                        };
                        writeln!(out, "  [R{} · {}{}] {}", m.round, m.sender_agent, v, m.body)?;
                    }
                }
            } else {
                let tasks = herd::list_tasks(&store)?;
                if as_json {
                    let rows: Vec<_> = tasks.iter().map(|t| t.to_json(None)).collect();
                    writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
                } else if tasks.is_empty() {
                    writeln!(
                        out,
                        "No active Herd parent-child tasks. Spawn one with `ghostback delegate --to codex` or press `p` in `ghostback ui`."
                    )?;
                } else {
                    for t in tasks {
                        writeln!(
                            out,
                            "{:<10} {:<14} {} ⇄ {} ({}) · R{}\n  parent: {}\n  child:  {}\n  goal:   {}",
                            t.task_id,
                            t.status.as_str(),
                            t.parent_agent.map(Agent::name).unwrap_or("parent"),
                            t.child_agent.name(),
                            t.role.label(),
                            t.round,
                            t.parent_terminal_key,
                            t.child_terminal_key,
                            t.goal
                        )?;
                    }
                }
            }
        }
    }
    out.flush()?;
    Ok(0)
}

fn is_broken_pipe(error: &Error) -> bool {
    error
        .downcast_ref::<io::Error>()
        .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(cli, &mut io::stdout().lock()) {
        Ok(code) => std::process::ExitCode::from(code.clamp(0, 255) as u8),
        Err(error) if is_broken_pipe(&error) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "ghostback: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
