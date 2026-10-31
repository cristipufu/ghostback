# Agent compatibility

Ghostback restores conversations in native Ghostty 1.4+ terminals on macOS with
zsh or bash. Agent authentication and conversation storage remain with each CLI.

| Agent | Capture | Exact recovery target |
| --- | --- | --- |
| Claude Code | Lifecycle hooks and process registry | `claude --resume <uuid>` |
| Codex | Dedicated interactive server and lifecycle hooks | `codex resume <uuid>` |
| Antigravity (`agy`) | Lifecycle hooks and process-owned log | `agy --conversation <uuid>` |
| Grok Build | Registry with PID and process birth time | `grok --resume <uuid>` |
| Pi | Interactive extension reading its session manager | `pi --session <absolute-file>` |
| OpenCode | TUI plugin reading its selected session route | `opencode --session <id>` |

## Pi

The integration is verified with `@earendil-works/pi-coding-agent` **1.4.0**.
Setup installs `~/.pi/agent/extensions/ghostback.js`, respecting an absolute
`PI_CODING_AGENT_DIR` override. Start Pi after setup so it loads the extension.
The bridge observes startup, reload, new/resumed/forked sessions, and checks
selection metadata every second. A heartbeat repairs missed transient captures.

Ghostback stores the exact, case-sensitive session ID and absolute file path.
`--session-dir` and `PI_CODING_AGENT_SESSION_DIR` can place sessions elsewhere;
there is no directory scan or most-recent-file lookup. The saved file header must
still match its ID and working directory before recovery. Missing, replaced,
unreadable, oversized, or non-regular session files require attention.

Only persisted interactive TUI sessions are recoverable. An empty session may
not have been written by Pi yet; ephemeral `--no-session` sessions are not saved.
`--print`, RPC/JSON modes, and disabled extensions are outside the integration.
Pi's in-memory `/tree` cursor, editor buffer, and viewport are not checkpointed;
Pi chooses the file's branch when reloading, as with its own resume command.

Supported launch options include provider, model, thinking level, and session
directory. Initial prompts, prior resume targets, and arbitrary extra arguments
are not replayed. Pi's existing runtime runs the bridge; Ghostback adds no Node
or Python dependency.

The adapter follows Pi's [extension API](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md)
and [session CLI](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/cli.md).

## OpenCode

The integration is verified with `opencode-ai` **1.19.5** and its TUI plugin API.
Setup adds one local file-URL plugin entry to `~/.config/opencode/tui.json`, or
`tui.jsonc` when present. It respects `XDG_CONFIG_HOME` and an absolute
`OPENCODE_CONFIG_DIR`. Existing comments, settings, and plugins survive setup
and uninstall. The bridge itself is owned under Ghostback's state directory.
Restart OpenCode after setup.

The plugin reads the selected session from the TUI route and uses that session's
own directory. Activity in another session on the backend cannot select it.
The home screen clears the recoverable selection. Loading states and unrelated
plugin routes do not manufacture a conversation ID.

This release supports the normal **local interactive TUI**. Remote `attach`,
headless `run`/`serve`/ACP, the `--mini` interface, `--pure`, and disabled TUI
plugins are outside this integration. Remote server addresses or passwords are
not stored. An existing project can disable a global plugin; check `ghostback
status` after launching with custom configuration.

Supported options include model, agent, and `--auto`. Resume/continue/fork
selectors are replaced by the captured session's exact ID. Prompt arguments
are never replayed.

The adapter follows the versioned [TUI plugin API](https://github.com/anomalyco/opencode/blob/v1.19.5/packages/plugin/src/tui.ts)
and [TUI config loader](https://github.com/anomalyco/opencode/blob/v1.19.5/packages/opencode/src/config/tui.ts).

## Upgrading an existing installation

```sh
brew update
brew upgrade ghostback
ghostback setup
```

Open a new Ghostty shell and restart Pi/OpenCode, then check `ghostback status`.
Pi and OpenCode need their bridges loaded before they can be captured. Setup is
idempotent and refuses to overwrite modified or unowned bridge files. Schema v3
keeps previous conversations; older Ghostback binaries refuse the newer schema
rather than modifying it.
