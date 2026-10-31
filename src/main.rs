use clap::{Parser, Subcommand};
use ghostback::sessions::{Agent, Session, Store, default_state_dir, hook_session};
use ghostback::{Error, Result, app, install, invalid, macos, process};
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
        Commands::Hook { agent } => {
            let mut bytes = Vec::new();
            io::stdin()
                .take(MAX_HOOK_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_HOOK_BYTES {
                return Err(invalid("hook payload exceeds the 1 MiB limit"));
            }
            let payload = serde_json::from_slice(&bytes)?;
            // Validate before consulting Ghostty; malformed hooks cannot cause automation.
            if std::env::var_os("GHOSTTY_SURFACE_ID").is_some()
                || std::env::var_os("GHOSTBACK_HOST_KEY").is_some()
            {
                app::register_hook(&store, agent, payload)?;
            } else {
                if !ghostback::sessions::unselected(agent, &payload)? {
                    hook_session(agent, payload, "unbound")?;
                }
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
