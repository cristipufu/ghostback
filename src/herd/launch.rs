//! Per-session MCP configuration, model/effort flag translation, and harness launch commands.
use crate::herd::TaskRole;
use crate::sessions::{Agent, shell_quote};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentLaunchOptions {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub extra_args: Vec<String>,
}

impl AgentLaunchOptions {
    /// Merges explicit launch options with inline `@model [effort]` options extracted from a prompt.
    pub fn merged_with_inline(self, inline: Self) -> Self {
        let mut extra_args = inline.extra_args;
        extra_args.extend(self.extra_args);
        Self {
            model: self.model.or(inline.model),
            effort: self.effort.or(inline.effort),
            extra_args,
        }
    }
}

const KNOWN_EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max", "off"];

/// Parses model and reasoning effort, supporting both explicit fields (`model="sol-6.1"`, `effort="xhigh"`)
/// and natural-language combined strings (`model="sol 6.1 xhigh"`, `effort=None`).
pub fn parse_model_and_effort(
    model: Option<&str>,
    effort: Option<&str>,
) -> (Option<String>, Option<String>) {
    let clean_token = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_control() && *c != '"' && *c != '\'')
            .collect::<String>()
            .trim()
            .to_owned()
    };

    let mut resolved_effort = effort
        .map(clean_token)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase());

    let raw_model = model.map(clean_token).unwrap_or_default();
    if raw_model.is_empty() {
        return (None, resolved_effort);
    }

    let mut parts: Vec<&str> = raw_model.split_whitespace().collect();
    if resolved_effort.is_none()
        && parts.len() >= 2
        && let Some(&last) = parts.last()
    {
        let lower = last.to_ascii_lowercase();
        if KNOWN_EFFORTS.contains(&lower.as_str()) {
            resolved_effort = Some(lower);
            parts.pop();
        }
    }

    let normalized_model = parts.join("-");
    let resolved_model = if normalized_model.is_empty() {
        None
    } else {
        Some(normalized_model)
    };
    (resolved_model, resolved_effort)
}

/// Extracts an optional leading `@<model> [effort]` prefix from a prompt/goal string.
/// For example, `"@sol-6.1 xhigh Review my diff"` -> `(opts{model: Some("sol-6.1"), effort: Some("xhigh")}, "Review my diff")`.
pub fn extract_inline_launch_options(raw_goal: &str) -> (AgentLaunchOptions, String) {
    extract_inline_launch_options_for_role(raw_goal, TaskRole::Reviewer)
}

/// Extracts an optional leading `@<model> [effort]` prefix from a prompt/goal string,
/// falling back to `role.default_goal()` when no explicit prompt text follows the prefix.
pub fn extract_inline_launch_options_for_role(
    raw_goal: &str,
    role: TaskRole,
) -> (AgentLaunchOptions, String) {
    let trimmed = raw_goal.trim();
    let Some(rest) = trimmed.strip_prefix('@') else {
        return (AgentLaunchOptions::default(), trimmed.to_owned());
    };
    let mut parts = rest.split_whitespace();
    let Some(first_token) = parts.next() else {
        return (AgentLaunchOptions::default(), trimmed.to_owned());
    };
    let mut model_tokens = vec![first_token];
    let mut effort: Option<String> = None;

    let tokens: Vec<&str> = parts.collect();
    let mut consumed = 0;
    if let Some(&t0) = tokens.first()
        && t0.chars().all(|c| c.is_ascii_digit() || c == '.')
    {
        model_tokens.push(t0);
        consumed += 1;
    }
    if let Some(&t_eff) = tokens.get(consumed) {
        let lower = t_eff.to_ascii_lowercase();
        if KNOWN_EFFORTS.contains(&lower.as_str()) {
            effort = Some(lower);
            consumed += 1;
        }
    }
    let remaining_text = tokens[consumed..].join(" ");
    let raw_model = model_tokens.join(" ");
    let (model, parsed_effort) = parse_model_and_effort(Some(&raw_model), effort.as_deref());
    let goal = if remaining_text.trim().is_empty() {
        role.default_goal().to_owned()
    } else {
        remaining_text
    };
    (
        AgentLaunchOptions {
            model,
            effort: parsed_effort,
            extra_args: Vec::new(),
        },
        goal,
    )
}

/// Translates `AgentLaunchOptions` (`model`, `effort`, `extra_args`) into native CLI flags for the target harness.
/// Note: Top-level interactive `opencode` does not accept `--model` on the CLI; its model is injected via `OPENCODE_CONFIG_CONTENT`.
pub fn build_agent_model_effort_args(agent: Agent, opts: &AgentLaunchOptions) -> Vec<String> {
    let (model, effort) = parse_model_and_effort(opts.model.as_deref(), opts.effort.as_deref());
    let mut args = Vec::new();
    match agent {
        Agent::Codex => {
            if let Some(m) = model {
                args.push("-m".into());
                args.push(m);
            }
            if let Some(e) = effort {
                args.push("-c".into());
                args.push(format!("model_reasoning_effort=\"{e}\""));
            }
        }
        Agent::Claude | Agent::Agy => {
            if let Some(m) = model {
                args.push("--model".into());
                args.push(m);
            }
            if let Some(e) = effort {
                args.push("--effort".into());
                args.push(e);
            }
        }
        Agent::Opencode => {
            // Top-level interactive `opencode` configures model via `OPENCODE_CONFIG_CONTENT` JSON.
        }
        Agent::Pi => {
            if let Some(m) = model {
                args.push("--model".into());
                args.push(m);
            }
            if let Some(e) = effort {
                args.push("--thinking".into());
                args.push(e);
            }
        }
        Agent::Grok => {
            if let Some(m) = model {
                args.push("--model".into());
                args.push(m);
            }
            if let Some(e) = effort {
                args.push("--reasoning-effort".into());
                args.push(e);
            }
        }
    }
    for extra in &opts.extra_args {
        let clean: String = extra.chars().filter(|c| !c.is_control()).collect();
        let trimmed = clean.trim();
        if !trimmed.is_empty() {
            args.push(trimmed.to_owned());
        }
    }
    args
}

pub fn mcp_config_json(exe: &Path, state_dir: &Path) -> Value {
    mcp_config_json_with_env(exe, state_dir, &[])
}

pub fn mcp_config_json_with_env(exe: &Path, state_dir: &Path, extra_env: &[(&str, &str)]) -> Value {
    let mut env_map = serde_json::Map::new();
    env_map.insert(
        "GHOSTBACK_STATE_DIR".to_owned(),
        Value::String(state_dir.to_string_lossy().into_owned()),
    );
    for &(k, v) in extra_env {
        env_map.insert(k.to_owned(), Value::String(v.to_owned()));
    }
    json!({
        "mcpServers": {
            "ghostback": {
                "command": exe.to_string_lossy(),
                "args": ["--state-dir", state_dir.to_string_lossy(), "mcp"],
                "env": env_map
            }
        }
    })
}

pub fn ensure_agy_workspace_dir(
    exe: &Path,
    state_dir: &Path,
    task_id: Option<&str>,
    extra_env: &[(&str, &str)],
) -> Option<PathBuf> {
    let ws_name = match task_id {
        Some(tid) if !tid.is_empty() => format!("agy-ws-{tid}"),
        _ => "agy-workspace".to_owned(),
    };
    let ws_dir = state_dir.join(ws_name);
    let plugin_dir = ws_dir.join(".agents").join("plugins").join("ghostback");
    let _ = std::fs::create_dir_all(ws_dir.join(".git"));
    let _ = std::fs::create_dir_all(&plugin_dir);
    let _ = std::fs::write(
        ws_dir.join("AGENTS.md"),
        orchestration_instructions(exe, state_dir),
    );
    let _ = std::fs::write(
        plugin_dir.join("plugin.json"),
        r#"{"name":"ghostback","version":"0.1.0","description":"Ghostback Herd Cross-Harness Orchestration"}"#,
    );
    if let Ok(mcp_body) =
        serde_json::to_string_pretty(&mcp_config_json_with_env(exe, state_dir, extra_env))
    {
        let _ = std::fs::write(plugin_dir.join("mcp_config.json"), mcp_body);
    }
    Some(ws_dir)
}

pub fn ensure_mcp_config_file(exe: &Path, state_dir: &Path) -> Option<PathBuf> {
    ensure_mcp_config_file_for_task(exe, state_dir, None, None)
}

pub fn ensure_mcp_config_file_for_task(
    exe: &Path,
    state_dir: &Path,
    task_id: Option<&str>,
    parent_key: Option<&str>,
) -> Option<PathBuf> {
    let _ = std::fs::create_dir_all(state_dir);
    let mut extra_env = Vec::new();
    if let Some(tid) = task_id.filter(|s| !s.is_empty()) {
        extra_env.push(("GHOSTBACK_TASK_ID", tid));
    }
    if let Some(pk) = parent_key.filter(|s| !s.is_empty()) {
        extra_env.push(("GHOSTBACK_PARENT_KEY", pk));
    }
    let _ = ensure_agy_workspace_dir(exe, state_dir, task_id, &extra_env);
    let filename = match task_id.filter(|s| !s.is_empty()) {
        Some(tid) => format!("mcp-task-{tid}.json"),
        None => "mcp-config.json".to_owned(),
    };
    let path = state_dir.join(filename);
    let body =
        serde_json::to_string_pretty(&mcp_config_json_with_env(exe, state_dir, &extra_env)).ok()?;
    std::fs::write(&path, body).ok()?;
    Some(path)
}

pub fn orchestration_instructions(exe: &Path, state_dir: &Path) -> String {
    let gb_cmd = format!(
        "{} --state-dir {}",
        shell_quote(&exe.to_string_lossy()),
        shell_quote(&state_dir.to_string_lossy())
    );
    format!(
        "[Ghostback Herd · Cross-Harness Agent Orchestration]\n\
         You have native Ghostback MCP tools (`ghostback_delegate`, `ghostback_ask`, `ghostback_reply`, `ghostback_wait`, `ghostback_tasks`) and the `ghostback` CLI.\n\
         - When the user asks you to use another agent, harness, or model for code review, implementation, or advice \
         (for example: \"use codex sol 6.1 xhigh for review\" or \"use grok as a reviewer\"), \
         call the `ghostback_delegate` tool with `agent` (\"codex\", \"claude\", \"agy\", \"opencode\", \"pi\", or \"grok\"), \
         `role` (\"reviewer\", \"worker\", or \"advisor\"), `model` (e.g. \"sol-6.1\"), `effort` (e.g. \"xhigh\"), `prompt`, and `wait: true` \
         (or run `{gb_cmd} delegate --to <agent> --role <role> --model <model> --effort <effort> --wait \"<prompt>\"`).\n\
         - If a child reviewer replies with `changes_requested`, fix the reported issues and call `ghostback_ask` \
         (or `{gb_cmd} ask --task <task_id> --wait \"<summary of fixes>\"`) until the child replies with `approved`.\n\
         - If a wait window finishes while the peer agent is still working, call `ghostback_wait` (or `{gb_cmd} wait --task <task_id>`) to continue waiting."
    )
}

/// Returns `(cli_args, env_pairs)` that inject the Ghostback MCP server and orchestration instructions
/// purely into the launched session without touching global user configuration files.
pub fn per_session_mcp_args_and_env(
    exe: &Path,
    state_dir: &Path,
    agent: Agent,
) -> (Vec<String>, Vec<(String, String)>) {
    per_session_mcp_args_and_env_with_options(
        exe,
        state_dir,
        agent,
        &AgentLaunchOptions::default(),
        None,
        None,
    )
}

pub fn per_session_mcp_args_and_env_with_options(
    exe: &Path,
    state_dir: &Path,
    agent: Agent,
    opts: &AgentLaunchOptions,
    task_id: Option<&str>,
    parent_key: Option<&str>,
) -> (Vec<String>, Vec<(String, String)>) {
    let mcp_filename = match task_id.filter(|s| !s.is_empty()) {
        Some(tid) => format!("mcp-task-{tid}.json"),
        None => "mcp-config.json".to_owned(),
    };
    let mcp_path = state_dir.join(mcp_filename);
    let instructions = orchestration_instructions(exe, state_dir);
    let (resolved_model, _) = parse_model_and_effort(opts.model.as_deref(), opts.effort.as_deref());
    let mut args = Vec::new();
    let mut envs = vec![
        (
            "GHOSTBACK_STATE_DIR".to_owned(),
            state_dir.to_string_lossy().into_owned(),
        ),
        (
            "GHOSTBACK_MCP_CONFIG".to_owned(),
            mcp_path.to_string_lossy().into_owned(),
        ),
    ];
    if task_id.filter(|s| !s.is_empty()).is_none() {
        envs.push(("GHOSTBACK_PARENT_AGENT".to_owned(), agent.name().to_owned()));
    }

    match agent {
        Agent::Claude => {
            args.push("--mcp-config".into());
            args.push(mcp_path.to_string_lossy().into_owned());
            args.push("--allowedTools".into());
            args.push("mcp__ghostback__*,Bash(*ghostback*)".into());
            args.push("--append-system-prompt".into());
            args.push(instructions);
        }
        Agent::Codex => {
            let cmd_toml = serde_json::to_string(&exe.to_string_lossy().as_ref())
                .unwrap_or_else(|_| "\"ghostback\"".into());
            let args_toml = serde_json::to_string(&vec![
                "--state-dir".to_owned(),
                state_dir.to_string_lossy().into_owned(),
                "mcp".to_owned(),
            ])
            .unwrap_or_else(|_| "[\"mcp\"]".into());
            let state_toml = serde_json::to_string(&state_dir.to_string_lossy().as_ref())
                .unwrap_or_else(|_| "\"\"".into());
            let instr_toml = serde_json::to_string(&instructions).unwrap_or_else(|_| "\"\"".into());
            args.push("-c".into());
            args.push(format!("mcp_servers.ghostback.command={cmd_toml}"));
            args.push("-c".into());
            args.push(format!("mcp_servers.ghostback.args={args_toml}"));
            args.push("-c".into());
            args.push(format!(
                "mcp_servers.ghostback.env.GHOSTBACK_STATE_DIR={state_toml}"
            ));
            args.push("-c".into());
            args.push("mcp_servers.ghostback.tool_timeout_sec=1800".into());
            args.push("-c".into());
            args.push("mcp_servers.ghostback.startup_timeout_sec=30".into());
            if let Some(tid) = task_id.filter(|s| !s.is_empty())
                && let Ok(tid_toml) = serde_json::to_string(tid)
            {
                args.push("-c".into());
                args.push(format!(
                    "mcp_servers.ghostback.env.GHOSTBACK_TASK_ID={tid_toml}"
                ));
                args.push("-a".into());
                args.push("never".into());
            } else {
                args.push("-c".into());
                args.push("mcp_servers.ghostback.env.GHOSTBACK_PARENT_AGENT=\"codex\"".into());
            }
            if let Some(pk) = parent_key.filter(|s| !s.is_empty())
                && let Ok(pk_toml) = serde_json::to_string(pk)
            {
                args.push("-c".into());
                args.push(format!(
                    "mcp_servers.ghostback.env.GHOSTBACK_PARENT_KEY={pk_toml}"
                ));
            }
            args.push("-c".into());
            args.push(format!("developer_instructions={instr_toml}"));
        }
        Agent::Agy => {
            let ws_name = match task_id.filter(|s| !s.is_empty()) {
                Some(tid) => format!("agy-ws-{tid}"),
                None => "agy-workspace".to_owned(),
            };
            let ws_dir = state_dir.join(ws_name);
            args.push("--add-dir".into());
            args.push(ws_dir.to_string_lossy().into_owned());
        }
        Agent::Pi => {
            args.push("--append-system-prompt".into());
            args.push(instructions);
        }
        Agent::Opencode => {
            let mut env_obj = serde_json::Map::new();
            env_obj.insert(
                "GHOSTBACK_STATE_DIR".to_owned(),
                Value::String(state_dir.to_string_lossy().into_owned()),
            );
            if let Some(tid) = task_id.filter(|s| !s.is_empty()) {
                env_obj.insert(
                    "GHOSTBACK_TASK_ID".to_owned(),
                    Value::String(tid.to_owned()),
                );
            } else {
                env_obj.insert(
                    "GHOSTBACK_PARENT_AGENT".to_owned(),
                    Value::String("opencode".to_owned()),
                );
            }
            if let Some(pk) = parent_key.filter(|s| !s.is_empty()) {
                env_obj.insert(
                    "GHOSTBACK_PARENT_KEY".to_owned(),
                    Value::String(pk.to_owned()),
                );
            }
            let mut oc_root = serde_json::Map::new();
            if let Some(m) = resolved_model {
                oc_root.insert("model".to_owned(), Value::String(m));
            }
            oc_root.insert(
                "mcp".to_owned(),
                json!({
                    "ghostback": {
                        "type": "local",
                        "command": [
                            exe.to_string_lossy(),
                            "--state-dir",
                            state_dir.to_string_lossy(),
                            "mcp"
                        ],
                        "environment": env_obj,
                        "enabled": true
                    }
                }),
            );
            envs.push((
                "OPENCODE_CONFIG_CONTENT".to_owned(),
                Value::Object(oc_root).to_string(),
            ));
        }
        Agent::Grok => {
            args.push("--rules".into());
            args.push(instructions);
            if task_id.filter(|s| !s.is_empty()).is_some() {
                args.push("--always-approve".into());
            }
        }
    }
    (args, envs)
}

fn user_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| s.starts_with('/') && !s.chars().any(char::is_control))
        .unwrap_or_else(|| "/bin/zsh".into())
}

/// Builds a shell launch command for a Parent agent started from `ghostback ui` / `--dock`,
/// automatically injecting the per-session Ghostback MCP server, instructions, and optional model/effort flags.
pub fn build_parent_launch_command(
    exe: &Path,
    state_dir: &Path,
    agent: Agent,
    opts: &AgentLaunchOptions,
) -> String {
    let (mcp_args, mcp_envs) =
        per_session_mcp_args_and_env_with_options(exe, state_dir, agent, opts, None, None);
    let model_args = build_agent_model_effort_args(agent, opts);
    let mut quoted_args = Vec::new();
    for a in mcp_args.iter().chain(model_args.iter()) {
        quoted_args.push(shell_quote(a));
    }
    let flags_str = if quoted_args.is_empty() {
        String::new()
    } else {
        format!(" {}", quoted_args.join(" "))
    };
    let agent_invocation = format!("{}{flags_str}", agent.name());
    let shell = user_shell();
    let exe_dir = exe
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut exports = vec![format!("PATH={}:$PATH", shell_quote(&exe_dir))];
    for (k, v) in mcp_envs {
        exports.push(format!("{k}={}", shell_quote(&v)));
    }
    let inner = format!(
        "export {}; {agent_invocation}; exec {}",
        exports.join(" "),
        shell_quote(&shell)
    );
    format!("{} -lic {}", shell_quote(&shell), shell_quote(&inner))
}

pub fn build_child_bootstrap_prompt(
    exe: &Path,
    state_dir: &Path,
    task_id: &str,
    parent_agent: Option<Agent>,
    child_agent: Agent,
    role: TaskRole,
    goal: &str,
) -> String {
    let parent_label = parent_agent
        .map(|a| a.name().to_ascii_uppercase())
        .unwrap_or_else(|| "PARENT".into());
    let child_label = child_agent.name().to_ascii_uppercase();
    let role_label = role.label().to_ascii_uppercase();
    let gb_cmd = format!(
        "{} --state-dir {}",
        shell_quote(&exe.to_string_lossy()),
        shell_quote(&state_dir.to_string_lossy())
    );
    format!(
        "[Ghostback Herd · Cross-Harness Task {task_id}]\n\
         Parent Harness: {parent_label}\n\
         Your Harness & Role: {child_label} ({role_label})\n\n\
         Task / Brief from {parent_label}:\n\
         {goal}\n\n\
         Communication Protocol (talk directly with {parent_label} until done):\n\
         1. Inspect the repository, git diff, or tests needed to fulfill your {role_label} role.\n\
         2. To request changes, ask a question, or send findings to {parent_label} AND wait for {parent_label}'s next revision, \
         call the `ghostback_reply` MCP tool with `task_id: \"{task_id}\"`, `status: \"changes_requested\"`, `wait: true`, or run:\n\
            {gb_cmd} reply --task {task_id} --status changes_requested --wait \"<your detailed findings>\"\n\
         3. When all requirements are satisfied and the work is approved/complete, finish the loop via `ghostback_reply` (`status: \"approved\"`, `wait: false`) or:\n\
            {gb_cmd} reply --task {task_id} --status approved \"<your final sign-off summary>\""
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_child_launch_command(
    exe: &Path,
    state_dir: &Path,
    task_id: &str,
    parent_terminal_key: &str,
    parent_agent: Option<Agent>,
    child_agent: Agent,
    role: TaskRole,
    goal: &str,
) -> String {
    build_child_launch_command_with_options(
        exe,
        state_dir,
        task_id,
        parent_terminal_key,
        parent_agent,
        child_agent,
        role,
        goal,
        &AgentLaunchOptions::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_child_launch_command_with_options(
    exe: &Path,
    state_dir: &Path,
    task_id: &str,
    parent_terminal_key: &str,
    parent_agent: Option<Agent>,
    child_agent: Agent,
    role: TaskRole,
    goal: &str,
    opts: &AgentLaunchOptions,
) -> String {
    let prompt = build_child_bootstrap_prompt(
        exe,
        state_dir,
        task_id,
        parent_agent,
        child_agent,
        role,
        goal,
    );
    let quoted_prompt = shell_quote(&prompt);
    let (mcp_args, mcp_envs) = per_session_mcp_args_and_env_with_options(
        exe,
        state_dir,
        child_agent,
        opts,
        Some(task_id),
        Some(parent_terminal_key),
    );
    let model_args = build_agent_model_effort_args(child_agent, opts);
    let mut quoted_flags = Vec::new();
    for a in mcp_args.iter().chain(model_args.iter()) {
        quoted_flags.push(shell_quote(a));
    }
    let flags_prefix = if quoted_flags.is_empty() {
        String::new()
    } else {
        format!("{} ", quoted_flags.join(" "))
    };

    let agent_invocation = match child_agent {
        Agent::Claude => format!("claude {flags_prefix}{quoted_prompt}"),
        Agent::Codex => format!("codex {flags_prefix}{quoted_prompt}"),
        Agent::Agy => format!("agy {flags_prefix}--prompt-interactive {quoted_prompt}"),
        Agent::Opencode => format!("opencode {flags_prefix}--prompt {quoted_prompt}"),
        Agent::Pi => format!("pi {flags_prefix}{quoted_prompt}"),
        Agent::Grok => format!("grok {flags_prefix}{quoted_prompt}"),
    };
    let shell = user_shell();
    let exe_dir = exe
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut exports = vec![
        format!("PATH={}:$PATH", shell_quote(&exe_dir)),
        format!("GHOSTBACK_TASK_ID={}", shell_quote(task_id)),
        format!("GHOSTBACK_PARENT_KEY={}", shell_quote(parent_terminal_key)),
    ];
    for (k, v) in mcp_envs {
        exports.push(format!("{k}={}", shell_quote(&v)));
    }
    let inner = format!(
        "export {}; {agent_invocation}; exec {}",
        exports.join(" "),
        shell_quote(&shell),
    );
    format!("{} -lic {}", shell_quote(&shell), shell_quote(&inner))
}

pub fn format_child_terminal_title(role: TaskRole, child_agent: Agent, task_id: &str) -> String {
    format!(
        "↳ {}:{} #{}",
        role.short_badge(),
        child_agent.name().to_ascii_uppercase(),
        task_id
    )
}
