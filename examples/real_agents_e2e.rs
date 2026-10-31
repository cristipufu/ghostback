//! Opt-in real CLI recovery test, isolated config and own Ghostty windows, no prompts/LLM calls.
use ghostback::sessions::{Agent, Session, Store, shell_quote};
use ghostback::{Result, app, files, install, invalid, process};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use uuid::Uuid;

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Window(String);
impl Drop for Window {
    fn drop(&mut self) {
        let script = format!(
            "tell application \"Ghostty\" to close (first terminal whose id is {})",
            literal(&self.0)
        );
        let _ = process::output(
            "/usr/bin/osascript",
            &["-e", &script],
            Duration::from_secs(5),
        );
    }
}
fn literal(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
fn environment(root: &Path) -> Vec<(String, String)> {
    [
        ("HOME", root.to_owned()),
        ("ZDOTDIR", root.to_owned()),
        ("PI_CODING_AGENT_DIR", root.join(".pi/agent")),
        ("OPENCODE_CONFIG_DIR", root.join(".config/opencode")),
        ("XDG_CONFIG_HOME", root.join(".config")),
        ("XDG_DATA_HOME", root.join("data")),
        ("XDG_STATE_HOME", root.join("opencode-state")),
        ("XDG_CACHE_HOME", root.join("cache")),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.to_string_lossy().into_owned()))
    .chain([
        ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
        ("OPENCODE_DISABLE_AUTOUPDATE".into(), "1".into()),
        ("PI_OFFLINE".into(), "1".into()),
        ("PI_TELEMETRY".into(), "0".into()),
        ("OPENCODE_DISABLE_MODELS_FETCH".into(), "1".into()),
    ])
    .collect()
}
fn fixture(root: &Path, agent: Agent, executable: &Path) -> Result<Session> {
    if agent == Agent::Pi {
        let file = root.join("pi sessions/exact 'conversation'.jsonl");
        files::atomic_write(&file, format!("{}\n",json!({"type":"session","version":3,"id":"Ghostback.Test-Session_1","timestamp":"2026-10-31T00:00:00.000Z","cwd":root})).as_bytes())?;
        let mut session = Session::new(
            "pending".into(),
            agent,
            "Ghostback.Test-Session_1",
            root.into(),
        )?;
        session.session_file = Some(file);
        return Ok(session);
    }
    let log = root.join("server.log");
    let mut command = Command::new(executable);
    command
        .args(["serve", "--hostname", "127.0.0.1", "--port", "0"])
        .envs(environment(root))
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&log)?)
        .stderr(Stdio::null());
    let _server = Server(command.spawn()?);
    let start = Instant::now();
    let url = loop {
        let text = fs::read_to_string(&log)?;
        if let Some(url) = text.lines().find_map(|s| {
            s.split_once("http://127.0.0.1:")
                .map(|(_, p)| format!("http://127.0.0.1:{}", p.trim()))
        }) {
            break url;
        }
        if start.elapsed() > Duration::from_secs(30) {
            return Err(invalid(format!(
                "OpenCode fixture server did not start: {text}"
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let response = process::output(
        "/usr/bin/curl",
        &[
            "--fail",
            "--silent",
            "--max-time",
            "10",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "--data",
            r#"{"title":"Ghostback local recovery fixture"}"#,
            &format!("{url}/session"),
        ],
        Duration::from_secs(12),
    )?;
    let value: serde_json::Value = serde_json::from_str(&response)?;
    Session::new(
        "pending".into(),
        agent,
        value["id"]
            .as_str()
            .ok_or_else(|| invalid(format!("No session from fixture API: {response}")))?,
        root.into(),
    )
}
fn run(agent: Agent, binary: &Path) -> Result<()> {
    let root = tempfile::Builder::new()
        .prefix("ghostback-real-agent-")
        .tempdir()?;
    let root = root.path().canonicalize()?;
    let executable = app::executable(agent.name())
        .ok_or_else(|| invalid("install both real CLIs before running this test"))?;
    install::setup(&install::Options {
        home: root.clone(),
        state: root.join("state"),
        source: binary.into(),
        dry_run: false,
        launch: false,
    })?;
    let mut session = fixture(&root, agent, &executable)?;
    let gate = root.join("go");
    let command = format!(
        "/bin/zsh -f -c {}",
        shell_quote(&format!(
            "while [[ ! -e {} ]]; do /bin/sleep 0.1; done; /bin/zsh -d -i",
            shell_quote(&gate.to_string_lossy())
        ))
    );
    let env = environment(&root)
        .iter()
        .map(|(k, v)| literal(&format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join(",");
    let script = format!(
        r#"tell application "Ghostty"
set cfg to new surface configuration
set initial working directory of cfg to {}
set command of cfg to {}
set wait after command of cfg to true
set environment variables of cfg to {{{}}}
set ownWindow to new window with configuration cfg
set name override of selected tab of ownWindow to "Ghostback real CLI test"
return id of terminal 1 of selected tab of ownWindow
end tell"#,
        literal(&root.to_string_lossy()),
        literal(&command),
        env
    );
    let key = process::output(
        "/usr/bin/osascript",
        &["-e", &script],
        Duration::from_secs(10),
    )?
    .trim()
    .to_owned();
    let _window = Window(key.clone());
    session.terminal_key = Uuid::parse_str(&key)?.to_string();
    let store = Store::new(root.join("state"))?;
    let options = if agent == Agent::Pi {
        vec!["--thinking".into(), "low".into()]
    } else {
        vec!["--agent".into(), "build".into()]
    };
    store.capture(&ghostback::lifecycle::Capture {
        session: session.clone(),
        shell_key: "2147483647:1".into(),
        owner_key: "2147483647:1".into(),
        executable,
        options: options.clone(),
    })?;
    fs::write(gate, b"go")?;
    let start = Instant::now();
    loop {
        let binding = store
            .bindings()?
            .into_iter()
            .next()
            .ok_or_else(|| invalid("fixture binding disappeared"))?;
        if binding.phase == "active" && binding.owner_key != "2147483647:1" {
            if binding.session != session
                || !binding.claim_shell.is_empty()
                || binding.options != options
            {
                return Err(invalid("real CLI acknowledged another conversation"));
            }
            if binding
                .executable
                .file_name()
                .is_some_and(|n| n == "node" || n == "bun")
            {
                return Err(invalid("saved a runtime instead of the agent executable"));
            }
            let pid = binding
                .owner_key
                .split(':')
                .next()
                .ok_or_else(|| invalid("missing owner pid"))?
                .parse::<u32>()?;
            if ghostback::native::alive(&binding.owner_key) {
                // SAFETY: this exact process was spawned in this fixture's own terminal.
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
            println!(
                "{}: exact resume and live UI capture passed ({}, executable {})",
                agent.name(),
                session.session_id,
                binding.executable.display()
            );
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(40) {
            return Err(invalid(format!(
                "{} did not acknowledge: {} {}; fixture {}",
                agent.name(),
                binding.phase,
                binding.reason,
                root.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn main() -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(invalid("real-agent e2e requires macOS and Ghostty"));
    }
    let binary = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| "target/release/ghostback".into())
        .canonicalize()?;
    for agent in [Agent::Pi, Agent::Opencode] {
        run(agent, &binary)?;
    }
    Ok(())
}
