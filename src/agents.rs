//! Pure argument policy. Prompts and one-shot commands are never replayed.
use crate::sessions::Agent;
use crate::{Result, invalid};

#[derive(PartialEq, Eq)]
enum Kind {
    Value,
    Flag,
    SkipValue,
    SkipFlag,
    Stop,
    Unknown,
}
fn kind(agent: Agent, flag: &str) -> Kind {
    if matches!(
        flag,
        "--model" | "--effort" | "--agent" | "--permission-mode" | "--add-dir"
    ) || flag == "-m"
    {
        return Kind::Value;
    }
    if matches!(
        flag,
        "--resume"
            | "-r"
            | "--conversation"
            | "--session"
            | "--session-id"
            | "--log-file"
            | "--cd"
            | "-C"
            | "--mcp-config"
            | "--append-system-prompt"
            | "--system-prompt"
            | "--settings"
            | "--rules"
            | "--allowedTools"
            | "--allowed-tools"
            | "--allow"
    ) {
        return Kind::SkipValue;
    }
    if matches!(flag, "--continue") {
        return Kind::SkipFlag;
    }
    if flag == "--" {
        return Kind::Stop;
    }
    match agent {
        Agent::Pi => match flag {
            "--provider" | "--thinking" | "--session-dir" => Kind::Value,
            "--no-session" | "--no-extensions" => Kind::Stop,
            "--print" | "-p" | "--mode" | "--fork" => Kind::Stop,
            "-c" => Kind::SkipFlag,
            _ => Kind::Unknown,
        },
        Agent::Opencode => match flag {
            "--auto" => Kind::Flag,
            "--mini" | "--no-replay" | "--pure" | "--replay-limit" => Kind::Stop,
            "-s" => Kind::SkipValue,
            "-c" | "--fork" => Kind::SkipFlag,
            "--prompt" => Kind::Stop,
            _ => Kind::Unknown,
        },
        Agent::Codex => match flag {
            "--profile" | "-p" | "--sandbox" | "--ask-for-approval" | "-a" => Kind::Value,
            "-c" | "--config" => Kind::SkipValue,
            "--no-daemon"
            | "--no-alt-screen"
            | "--search"
            | "--approve-for-me"
            | "--dangerously-bypass-approvals-and-sandbox"
            | "--yolo" => Kind::Flag,
            "--worktree" => Kind::SkipFlag,
            _ => Kind::Unknown,
        },
        Agent::Agy => match flag {
            "--mode" => Kind::Value,
            "--sandbox" | "--dangerously-skip-permissions" => Kind::Flag,
            "--print" | "-p" | "--prompt" | "--prompt-interactive" | "-i" => Kind::Stop,
            "-c" | "--new-project" => Kind::SkipFlag,
            _ => Kind::Unknown,
        },
        Agent::Claude => match flag {
            "--dangerously-skip-permissions"
            | "--allow-dangerously-skip-permissions"
            | "--chrome"
            | "--no-chrome" => Kind::Flag,
            "--print" | "-p" => Kind::Stop,
            "-c" => Kind::SkipFlag,
            _ => Kind::Unknown,
        },
        Agent::Grok => match flag {
            "--reasoning-effort" | "--sandbox" => Kind::Value,
            "--always-approve" | "--no-plan" | "--no-subagents" | "--no-alt-screen"
            | "--fullscreen" => Kind::Flag,
            "--single" | "-p" | "--prompt-file" | "--prompt-json" => Kind::Stop,
            "-c" => Kind::SkipFlag,
            _ => Kind::Unknown,
        },
    }
}

pub fn replay_options(agent: Agent, argv: &[String]) -> Result<Vec<String>> {
    let mut options = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let arg = &argv[i];
        if agent == Agent::Codex && arg == "resume" {
            i += 2;
            continue;
        }
        if agent == Agent::Opencode && !arg.starts_with('-') {
            i += 1;
            continue;
        }
        let (flag, inline) = arg
            .split_once('=')
            .map(|(a, b)| (a, Some(b)))
            .unwrap_or((arg, None));
        match kind(agent, flag) {
            Kind::Value => {
                let value = if let Some(value) = inline {
                    value.to_owned()
                } else {
                    i += 1;
                    argv.get(i)
                        .cloned()
                        .ok_or_else(|| invalid("missing launch option value"))?
                };
                options.extend([flag.to_owned(), value]);
            }
            Kind::Flag => options.push(flag.to_owned()),
            Kind::SkipValue => {
                if inline.is_none() {
                    i += 1;
                }
            }
            Kind::SkipFlag => {}
            Kind::Stop | Kind::Unknown => break,
        }
        i += 1;
    }
    validate_options(agent, &options)?;
    Ok(options)
}

pub fn validate_options(agent: Agent, options: &[String]) -> Result<()> {
    if options.len() > 128
        || options
            .iter()
            .any(|s| s.len() > 8192 || s.chars().any(char::is_control))
    {
        return Err(invalid("invalid captured agent options"));
    }
    let mut i = 0;
    while i < options.len() {
        match kind(agent, &options[i]) {
            Kind::Value => {
                i += 1;
                if options.get(i).is_none_or(|s| s.is_empty()) {
                    return Err(invalid("saved option is missing its value"));
                }
            }
            Kind::Flag => {}
            _ => {
                return Err(invalid(
                    "saved launch arguments contain a prompt or unsupported option",
                ));
            }
        }
        i += 1;
    }
    Ok(())
}

pub fn interactive(agent: Agent, args: &[String]) -> bool {
    if agent == Agent::Pi
        && args
            .iter()
            .any(|a| matches!(a.as_str(), "--no-extensions" | "--mode") || a.starts_with("--mode="))
    {
        return false;
    }
    if agent == Agent::Opencode
        && args.iter().any(|a| {
            matches!(
                a.as_str(),
                "--pure" | "--mini" | "--no-replay" | "--replay-limit"
            )
        })
    {
        return false;
    }
    if agent == Agent::Codex
        && args
            .get(1)
            .is_some_and(|a| matches!(a.as_str(), "exec" | "app-server" | "doctor" | "review"))
    {
        return false;
    }
    let mut i = 1;
    while i < args.len() {
        let (flag, inline) = args[i]
            .split_once('=')
            .map(|(a, b)| (a, Some(b)))
            .unwrap_or((&args[i], None));
        if agent == Agent::Opencode
            && matches!(
                flag,
                "attach"
                    | "run"
                    | "serve"
                    | "web"
                    | "acp"
                    | "auth"
                    | "models"
                    | "session"
                    | "db"
                    | "export"
                    | "import"
                    | "upgrade"
                    | "uninstall"
                    | "mcp"
                    | "agent"
                    | "github"
                    | "pr"
                    | "completion"
                    | "debug"
                    | "providers"
                    | "stats"
                    | "plugin"
                    | "plug"
            )
        {
            return false;
        }
        if flag == "--" {
            break;
        }
        if match agent {
            Agent::Claude | Agent::Agy => matches!(flag, "--print" | "-p" | "--prompt"),
            Agent::Grok => matches!(flag, "--single" | "-p" | "--prompt-file" | "--prompt-json"),
            Agent::Pi => matches!(flag, "--print" | "-p"),
            Agent::Opencode | Agent::Codex => false,
        } {
            return false;
        }
        if matches!(kind(agent, flag), Kind::Value | Kind::SkipValue) && inline.is_none() {
            i += 1;
        }
        i += 1;
    }
    true
}
