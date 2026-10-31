//! Opt-in macOS integration test. Creates and closes only its own Ghostty window.
use ghostback::lifecycle::Capture;
use ghostback::sessions::{Agent, Session, Store, shell_quote};
use ghostback::{Result, invalid, native, process};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use uuid::Uuid;

fn literal(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
fn wait(path: &Path, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    while !path.is_file() {
        if start.elapsed() > timeout {
            return Err(invalid(format!("timed out waiting for {}", path.display())));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}
struct Window(Uuid);
impl Drop for Window {
    fn drop(&mut self) {
        let script = format!(
            "tell application \"Ghostty\" to close (first terminal whose id is {})",
            literal(&self.0.to_string().to_uppercase())
        );
        let _ = process::output(
            "/usr/bin/osascript",
            &["-e", &script],
            Duration::from_secs(5),
        );
    }
}

fn fake_agent() -> Result<()> {
    let root = PathBuf::from(
        std::env::var_os("GHOSTBACK_E2E_ROOT").ok_or_else(|| invalid("missing fixture root"))?,
    );
    let binary = std::env::var_os("GHOSTBACK_E2E_BINARY")
        .ok_or_else(|| invalid("missing fixture binary"))?;
    let agent = Agent::parse(
        std::env::current_exe()?
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid("missing agent name"))?,
    )?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mut expected = vec![
        agent.resume_flag().to_owned(),
        Uuid::from_u128(1).to_string(),
    ];
    if agent == Agent::Codex {
        expected.insert(0, "--no-daemon".into());
    }
    if args != expected {
        return Err(invalid("fake agent received incorrect resume arguments"));
    }
    fs::write(
        root.join("agent-arguments.json"),
        serde_json::to_vec(&args)?,
    )?;
    if agent == Agent::Grok {
        let owner = native::process(std::process::id())?;
        fs::create_dir_all(root.join(".grok"))?;
        fs::write(
            root.join(".grok/active_sessions.json"),
            serde_json::to_vec(&serde_json::json!([{
                "pid":owner.pid,"pid_start":owner.started_us,"session_id":Uuid::from_u128(1),"cwd":root
            }]))?,
        )?;
        fs::write(root.join("grok-pid"), owner.pid.to_string())?;
        let store = Store::new(root.join("state"))?;
        let start = Instant::now();
        while !store
            .bindings()?
            .iter()
            .any(|b| b.phase == "active" && b.owner_key == owner.key())
        {
            if start.elapsed() > Duration::from_secs(10) {
                return Err(invalid("Grok registry was not acknowledged"));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        return Ok(());
    }
    let payload = if agent == Agent::Agy {
        serde_json::json!({"conversationId":Uuid::from_u128(1),"workspacePaths":[root],"hook_event_name":"SessionStart"})
    } else {
        serde_json::json!({"session_id":Uuid::from_u128(1),"cwd":root,"hook_event_name":"SessionStart"})
    };
    let mut command = Command::new(binary);
    command
        .arg("--state-dir")
        .arg(root.join("state"))
        .args(["hook", agent.name()])
        .stdin(Stdio::piped());
    if agent == Agent::Claude {
        // Reproduce hooks from a session opened before setup: no hints and no child TTY.
        command
            .env_remove("GHOSTBACK_TERMINAL_KEY")
            .env_remove("GHOSTBACK_HOST_KEY");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: setsid is async-signal-safe and touches no Rust runtime state.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }
    }
    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| invalid("missing hook input"))?
        .write_all(&serde_json::to_vec(&payload)?)?;
    if !child.wait()?.success() {
        return Err(invalid("fake agent acknowledgement was rejected"));
    }
    Ok(())
}

fn run() -> Result<()> {
    if std::env::current_exe()?
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| Agent::parse(name).is_ok())
    {
        return fake_agent();
    }
    if !cfg!(target_os = "macos") {
        return Err(invalid("native e2e requires macOS with Ghostty running"));
    }
    let binary = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/release/ghostback"))
        .canonicalize()?;
    let agent = std::env::args()
        .nth(2)
        .map(|name| Agent::parse(&name))
        .transpose()?
        .unwrap_or(Agent::Claude);
    let root = tempfile::Builder::new()
        .prefix("ghostback-native-e2e-")
        .tempdir()?;
    let directory = root.path().canonicalize()?;
    fs::create_dir(directory.join("bin"))?;
    let fake = directory.join("bin").join(agent.name());
    fs::copy(std::env::current_exe()?, &fake)?;
    let old = directory.join("old-shell");
    let go = directory.join("go");
    let done = directory.join("done");
    let initial = format!(
        "printf '%s' \"$$\" > {}; for i in {{1..400}}; do [[ -e {} ]] && exit 0; /bin/sleep 0.025; done; exit 19",
        shell_quote(&old.to_string_lossy()),
        shell_quote(&go.to_string_lossy())
    );
    let bash = std::env::args().nth(3).as_deref() == Some("bash");
    let mut rc = ghostback::install::shell_body(&binary, &directory.join("state"), bash);
    if bash {
        rc.push_str(&format!(
            "\nPROMPT_COMMAND=\"$PROMPT_COMMAND; /usr/bin/touch {}\"\n",
            shell_quote(&done.to_string_lossy())
        ));
    } else {
        rc.push_str(&format!("\n_ghostback_e2e_done() {{ /usr/bin/touch {}; }}\nadd-zsh-hook precmd _ghostback_e2e_done\n",shell_quote(&done.to_string_lossy())));
    }
    let rc_path = directory.join(if bash { ".bashrc" } else { ".zshrc" });
    fs::write(&rc_path, rc)?;
    let restored = if bash {
        format!(
            "/bin/bash --noprofile --rcfile {} -i",
            shell_quote(&rc_path.to_string_lossy())
        )
    } else {
        "/bin/zsh -d -i".into()
    };
    let script = format!("/bin/zsh -f -c {}; {}", shell_quote(&initial), restored);
    let command = format!("/bin/zsh -f -c {}", shell_quote(&script));
    let source = format!(
        r#"tell application "Ghostty"
set cfg to new surface configuration
set initial working directory of cfg to {}
set command of cfg to {}
set wait after command of cfg to true
set environment variables of cfg to {{{},{},{},{},{}}}
set ownWindow to new window with configuration cfg
set name override of selected tab of ownWindow to "Ghostback integration test"
return id of terminal 1 of selected tab of ownWindow
end tell"#,
        literal(&directory.to_string_lossy()),
        literal(&command),
        literal(&format!("GHOSTBACK_E2E_ROOT={}", directory.display())),
        literal(&format!("GHOSTBACK_E2E_BINARY={}", binary.display())),
        literal(&format!("HOME={}", directory.display())),
        literal(&format!("ZDOTDIR={}", directory.display())),
        literal(&format!(
            "PATH={}/bin:/usr/bin:/bin:/usr/sbin",
            directory.display()
        ))
    );
    let key = Uuid::parse_str(
        process::output(
            "/usr/bin/osascript",
            &["-e", &source],
            Duration::from_secs(10),
        )?
        .trim(),
    )?;
    let _window = Window(key);
    wait(&old, Duration::from_secs(10))?;
    let pid = fs::read_to_string(&old)?.parse::<u32>()?;
    let origin = native::process(pid)?;
    let store = Store::new(directory.join("state"))?;
    store.capture(&Capture {
        session: Session::new(
            key.to_string(),
            agent,
            &Uuid::from_u128(1).to_string(),
            directory.clone(),
        )?,
        shell_key: origin.key(),
        owner_key: "2147483647:1".into(),
        executable: fake,
        options: Vec::new(),
    })?;
    fs::write(go, b"continue")?;
    if agent == Agent::Grok {
        let pid_file = directory.join("grok-pid");
        wait(&pid_file, Duration::from_secs(10))?;
        let pid = fs::read_to_string(pid_file)?.parse()?;
        let owner = native::process(pid)?;
        let roots = ghostback::capture::Roots {
            claude: directory.join(".claude"),
            codex: directory.join(".codex"),
            grok: directory.join(".grok"),
        };
        let observed = ghostback::capture::resolve(
            &store,
            &roots,
            &owner,
            &key.to_string(),
            &directory,
            &mut ghostback::capture::Cache::default(),
        )?
        .ok_or_else(|| invalid("Grok registry capture failed"))?;
        store.capture(&observed)?;
    }
    wait(&done, Duration::from_secs(15))?;
    let args: Vec<String> =
        serde_json::from_slice(&fs::read(directory.join("agent-arguments.json"))?)?;
    let mut expected = vec![
        agent.resume_flag().to_owned(),
        Uuid::from_u128(1).to_string(),
    ];
    if agent == Agent::Codex {
        expected.insert(0, "--no-daemon".into());
    }
    if args != expected {
        return Err(invalid("incorrect arguments reached the agent"));
    }
    let binding = store
        .bindings()?
        .into_iter()
        .next()
        .ok_or_else(|| invalid("missing recovered binding"))?;
    if binding.phase != "retired" {
        return Err(invalid(format!(
            "recovery did not acknowledge and retire cleanly: {}: {}",
            binding.phase, binding.reason
        )));
    }
    println!(
        "PASS ({}, {}): native terminal UUID → generated shell integration → exact resume → acknowledgement → deliberate exit.",
        agent.name(),
        if bash { "bash" } else { "zsh" }
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("native-e2e: {error}");
        std::process::exit(1);
    }
}
