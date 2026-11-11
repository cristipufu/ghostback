use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use crate::sessions::{Agent, Store};
use crate::{Result, invalid};

use super::{
    AgentLaunchOptions, DelegateSpec, ReplyVerdict, TaskRole, delegate_task, get_task, list_tasks,
    orchestration_instructions, record_child_reply, record_parent_message,
    resolve_caller_terminal_key, resolve_task_id, wait_for_peer_formatted,
    wait_or_timeout_for_child, wait_or_timeout_for_parent,
};

fn mcp_tools_schema() -> Value {
    json!({
        "tools": [
            {
                "name": "ghostback_delegate",
                "description": "Spawn a Child agent from another harness (claude, codex, agy, opencode, pi, grok) in a Ghostty surface as a Reviewer, Worker, or Advisor, and optionally wait for its Round 1 reply. Use this when the user asks to delegate or review with another agent/model (e.g. 'use codex sol 6.1 xhigh for review' -> agent='codex', model='sol-6.1', effort='xhigh', role='reviewer', wait=true).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "agent": {
                            "type": "string",
                            "enum": ["claude", "codex", "agy", "opencode", "pi", "grok"],
                            "description": "Target child harness to launch."
                        },
                        "role": {
                            "type": "string",
                            "enum": ["reviewer", "worker", "advisor"],
                            "description": "Child agent role (defaults to 'reviewer')."
                        },
                        "prompt": {
                            "type": "string",
                            "description": "Detailed task brief or review instructions for the child agent."
                        },
                        "model": {
                            "type": "string",
                            "description": "Optional model name or slug for the child agent (e.g. 'sol-6.1', 'gpt-5.1-codex', 'claude-opus-4-6')."
                        },
                        "effort": {
                            "type": "string",
                            "enum": ["minimal", "low", "medium", "high", "xhigh", "max"],
                            "description": "Optional reasoning / thinking effort level for the child agent."
                        },
                        "extra_args": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional extra CLI flags to pass to the child harness."
                        },
                        "wait": {
                            "type": "boolean",
                            "description": "Wait for the child agent's reply before returning (defaults to true)."
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "Maximum seconds to wait when wait=true (defaults to 900)."
                        },
                        "no_spawn": {
                            "type": "boolean",
                            "description": "If true, record the task in SQLite without spawning a Ghostty surface (used in headless tests)."
                        }
                    },
                    "required": ["agent", "prompt"]
                }
            },
            {
                "name": "ghostback_ask",
                "description": "Send a follow-up message or next-round revision from the Parent agent to an existing Child task (e.g. after addressing changes_requested) and wait for the Child's re-review.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "Task ID (e.g. 't-1a2b3c4d'). Optional if only one task belongs to this parent terminal."
                        },
                        "message": {
                            "type": "string",
                            "description": "Message or summary of fixes to send to the child agent."
                        },
                        "wait": {
                            "type": "boolean",
                            "description": "Wait for the child agent's next reply (defaults to true)."
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "Maximum seconds to wait when wait=true (defaults to 900)."
                        }
                    },
                    "required": ["message"]
                }
            },
            {
                "name": "ghostback_reply",
                "description": "Send a reply or review verdict from a Child agent back to its Parent agent ('changes_requested', 'approved', 'done', or 'comment'). When status is 'changes_requested', wait defaults to true so this call blocks until the Parent sends its next revision.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "Task ID. Auto-resolved from GHOSTBACK_TASK_ID or child terminal if omitted."
                        },
                        "status": {
                            "type": "string",
                            "enum": ["changes_requested", "approved", "done", "comment"],
                            "description": "Verdict for this round."
                        },
                        "message": {
                            "type": "string",
                            "description": "Detailed review findings, answer, or sign-off message."
                        },
                        "wait": {
                            "type": "boolean",
                            "description": "Whether to wait for the Parent's next revision before returning."
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "Maximum seconds to wait when wait=true (defaults to 900)."
                        }
                    },
                    "required": ["status", "message"]
                }
            },
            {
                "name": "ghostback_wait",
                "description": "Wait for the peer agent ('child' or 'parent') to send its next message on a task without sending a duplicate message. If the peer has already responded, returns that latest response immediately.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "Task ID. Auto-resolved if omitted."
                        },
                        "for_sender": {
                            "type": "string",
                            "enum": ["child", "parent"],
                            "description": "Which peer to wait for ('child' when called by Parent, 'parent' when called by Child)."
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "Maximum seconds to wait (defaults to 900)."
                        }
                    }
                }
            },
            {
                "name": "ghostback_tasks",
                "description": "List active and recent Parent ↔ Child orchestration tasks and their message transcripts.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "Optional specific task ID to inspect."
                        },
                        "include_messages": {
                            "type": "boolean",
                            "description": "Include full message history (defaults to true)."
                        }
                    }
                }
            }
        ]
    })
}

fn execute_mcp_tool(store: &Store, name: &str, args: &Value) -> Result<String> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
    match name {
        "ghostback_delegate" => {
            let agent_str = args
                .get("agent")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing required 'agent' parameter"))?;
            let to = Agent::parse(agent_str)?;
            let role = args
                .get("role")
                .and_then(Value::as_str)
                .map(TaskRole::parse)
                .unwrap_or(TaskRole::Reviewer);
            let raw_prompt = args.get("prompt").and_then(Value::as_str).unwrap_or("");
            let options = AgentLaunchOptions {
                model: args.get("model").and_then(Value::as_str).map(str::to_owned),
                effort: args
                    .get("effort")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                extra_args: args
                    .get("extra_args")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
            };
            let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(true);
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(900)
                .clamp(1, 3600);
            let no_spawn = args
                .get("no_spawn")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
            let cwd = cwd.canonicalize().unwrap_or(cwd);

            let (task, seq, _) = delegate_task(
                store,
                &DelegateSpec {
                    exe: &exe,
                    parent_key: None,
                    parent_agent: None,
                    target_uuid: None,
                    child_agent: to,
                    role,
                    raw_prompt,
                    options,
                    cwd: &cwd,
                    no_spawn,
                    sidebar_cmd: None,
                },
            )?;
            if wait && !no_spawn {
                wait_or_timeout_for_parent(
                    store,
                    &exe,
                    task,
                    seq,
                    Duration::from_secs(timeout_secs),
                )
            } else {
                Ok(serde_json::to_string_pretty(&task.to_json(None))?)
            }
        }
        "ghostback_ask" => {
            let message = args
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing required 'message' parameter"))?;
            let explicit_task = args.get("task_id").and_then(Value::as_str);
            let current_key = resolve_caller_terminal_key(store);
            let task_id = resolve_task_id(store, explicit_task, current_key.as_deref(), false)?;
            let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(true);
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(900)
                .clamp(1, 3600);
            let (task, seq) = record_parent_message(store, &task_id, message)?;
            if wait {
                wait_or_timeout_for_parent(
                    store,
                    &exe,
                    task,
                    seq,
                    Duration::from_secs(timeout_secs),
                )
            } else {
                Ok(serde_json::to_string_pretty(&task.to_json(None))?)
            }
        }
        "ghostback_reply" => {
            let status_str = args
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing required 'status' parameter"))?;
            let verdict = ReplyVerdict::parse(status_str);
            let message = args
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("missing required 'message' parameter"))?;
            let explicit_task = args.get("task_id").and_then(Value::as_str);
            let current_key = resolve_caller_terminal_key(store);
            let task_id = resolve_task_id(store, explicit_task, current_key.as_deref(), true)?;
            let default_wait = !verdict.is_terminal();
            let wait = args
                .get("wait")
                .and_then(Value::as_bool)
                .unwrap_or(default_wait);
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(900)
                .clamp(1, 3600);
            let (task, seq) = record_child_reply(store, &task_id, verdict, message)?;
            if wait && !verdict.is_terminal() {
                wait_or_timeout_for_child(store, &exe, task, seq, Duration::from_secs(timeout_secs))
            } else {
                Ok(serde_json::to_string_pretty(&task.to_json(None))?)
            }
        }
        "ghostback_wait" => {
            let explicit_task = args.get("task_id").and_then(Value::as_str);
            let for_sender = args
                .get("for_sender")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_ascii_lowercase())
                .unwrap_or_else(|| {
                    if std::env::var_os("GHOSTBACK_TASK_ID").is_some() {
                        "parent".to_owned()
                    } else {
                        "child".to_owned()
                    }
                });
            let is_child = for_sender == "parent";
            let current_key = resolve_caller_terminal_key(store);
            let task_id = resolve_task_id(store, explicit_task, current_key.as_deref(), is_child)?;
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(900)
                .clamp(1, 3600);
            let expected = if is_child { "parent" } else { "child" };
            wait_for_peer_formatted(
                store,
                &exe,
                &task_id,
                expected,
                Duration::from_secs(timeout_secs),
            )
        }
        "ghostback_tasks" => {
            let include_messages = args
                .get("include_messages")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if let Some(tid) = args.get("task_id").and_then(Value::as_str) {
                let (task, msgs) = get_task(store, tid)?
                    .ok_or_else(|| invalid(format!("no task found with id {tid}")))?;
                let payload = if include_messages {
                    task.to_json(Some(&msgs))
                } else {
                    task.to_json(None)
                };
                Ok(serde_json::to_string_pretty(&payload)?)
            } else {
                let all = list_tasks(store)?;
                let mut rows = Vec::with_capacity(all.len());
                for t in all {
                    if include_messages {
                        let msgs = get_task(store, &t.task_id)?
                            .map(|(_, m)| m)
                            .unwrap_or_default();
                        rows.push(t.to_json(Some(&msgs)));
                    } else {
                        rows.push(t.to_json(None));
                    }
                }
                Ok(serde_json::to_string_pretty(&Value::Array(rows))?)
            }
        }
        other => Err(invalid(format!("unknown MCP tool: {other}"))),
    }
}

pub fn handle_mcp_request(store: &Store, req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(Value::as_str)?;
    let id = req.get("id").cloned();

    // Notifications have no `id` and expect no response
    if id.is_none() || method.starts_with("notifications/") {
        return None;
    }
    let id_val = id.unwrap_or(Value::Null);

    match method {
        "initialize" => {
            let client_proto = req
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05");
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
            Some(json!({
                "jsonrpc": "2.0",
                "id": id_val,
                "result": {
                    "protocolVersion": client_proto,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "ghostback",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "instructions": orchestration_instructions(&exe, &store.directory)
                }
            }))
        }
        "ping" => Some(json!({
            "jsonrpc": "2.0",
            "id": id_val,
            "result": {}
        })),
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id_val,
            "result": mcp_tools_schema()
        })),
        "tools/call" => {
            let params = req.get("params").unwrap_or(&Value::Null);
            let tool_name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").unwrap_or(&Value::Null);
            match execute_mcp_tool(store, tool_name, arguments) {
                Ok(text) => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id_val,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": text
                            }
                        ],
                        "isError": false
                    }
                })),
                Err(err) => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id_val,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": format!("Error: {err}")
                            }
                        ],
                        "isError": true
                    }
                })),
            }
        }
        _ => Some(json!({
            "jsonrpc": "2.0",
            "id": id_val,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {method}")
            }
        })),
    }
}

/// Runs the Ghostback MCP server over stdio, supporting both newline-delimited JSON-RPC
/// and `Content-Length:` framed JSON-RPC.
pub fn run_mcp_server(store: &Store, mut reader: impl BufRead, out: &mut impl Write) -> Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (payload, use_content_length) =
            if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
                let mut content_len: Option<usize> = None;
                let parse_cl = |hdr: &str| -> Option<usize> {
                    let (k, v) = hdr.split_once(':')?;
                    if k.trim().eq_ignore_ascii_case("content-length") {
                        v.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                };
                if let Some(len) = parse_cl(trimmed) {
                    content_len = Some(len);
                }
                loop {
                    line.clear();
                    if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
                        break;
                    }
                    if content_len.is_none() {
                        content_len = parse_cl(line.trim());
                    }
                }
                let len = content_len.ok_or_else(|| invalid("invalid Content-Length header"))?;
                if len > 8 * 1024 * 1024 {
                    return Err(invalid("MCP frame exceeds 8 MiB limit"));
                }
                let mut buf = vec![0u8; len];
                reader.read_exact(&mut buf)?;
                (String::from_utf8(buf)?, true)
            } else {
                (trimmed.to_owned(), false)
            };

        let Ok(req) = serde_json::from_str::<Value>(&payload) else {
            continue;
        };
        if let Some(resp) = handle_mcp_request(store, &req) {
            let encoded = serde_json::to_string(&resp)?;
            if use_content_length {
                write!(out, "Content-Length: {}\r\n\r\n{}", encoded.len(), encoded)?;
            } else {
                writeln!(out, "{encoded}")?;
            }
            out.flush()?;
        }
    }
    Ok(())
}
