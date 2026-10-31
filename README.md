<p align="center"><img src="docs/assets/hero.svg" alt="Ghostback — your coding sessions, right where you left them." width="100%" /></p>

<p align="center">
  <strong>Automatic coding-agent recovery for Ghostty.</strong><br />
  Native tabs. Exact conversations. A small Rust executable.
</p>

<p align="center">
  <a href="#get-started">Get started</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#when-recovery-needs-attention">Troubleshooting</a> ·
  <a href="CONTRIBUTING.md">Contributing</a>
</p>

---

You reopen Ghostty after a restart. Your tabs return, but the agents inside them
need to be resumed one by one. Ghostback remembers which conversation belongs
to each terminal and reconnects it when that terminal's shell is ready.

It supports **Claude Code, Codex, Antigravity (`agy`), and Grok Build** on
**macOS with Ghostty 1.4+ and zsh or bash**.

> **Pre-release.** The capture, installation, background helper, and restoration
> paths are implemented. Signed public downloads are not published yet. See
> [verification](docs/verification.md) for tested behavior and release qualification.

## Why Ghostback

- **The right conversation.** Exact IDs keep several sessions in the same
  repository distinct.
- **Your existing workspace.** Ghostty restores its own windows, tabs, and splits.
  Ghostback reconnects agents to those restored terminals.
- **A careful restore path.** Recovery runs from the shell prompt. It does not
  paste commands into a terminal where you may already be working.
- **Small and local.** A native executable with embedded SQLite. No Python, uv,
  Node, Rust toolchain, or separate runtime is required to run a packaged build.
- **Inspectable failures.** Failed or ambiguous recovery stays visible in
  `ghostback status`, with saved identity retained for a deliberate retry.

## Get started

Until release downloads are published, build from source:

```sh
cargo build --release --locked
./target/release/ghostback setup --dry-run
./target/release/ghostback setup
```

Setup installs the executable, a login helper, shell integration, and the agent
hooks it needs. It preserves existing configuration, keeps backups, and enables
Ghostty's native window restoration. Open a new Ghostty tab after setup.

Allow macOS Automation access when requested. Codex may also ask you to trust
its new lifecycle hook once. Run agents using their normal names:

```sh
claude
codex
agy
grok
```

Existing sessions started before setup may need one new launch before they can
be tracked. `ghostback status` reports which identities were captured.

When you reopen Ghostty, recovery runs automatically in the restored shells.
There is no per-tab `/resume` step in the normal recovery flow.

## How it works

```mermaid
flowchart LR
    A[Use your agents normally] --> B[Capture exact conversation + terminal IDs]
    B --> C[(Local SQLite state)]
    C --> D[Ghostty restores its native workspace]
    D --> E[Shell prompt claims its saved session]
    E --> F[Resume and verify the conversation]
```

Ghostback joins native terminal UUIDs to agent processes using their controlling
TTYs and process birth times. It reads process-owned session metadata or native
lifecycle events. A directory or tab title is never used as conversation identity.

The coordinator records a restore claim before launching anything. Another
prompt cannot launch the same claim again. A fresh agent observation acknowledges
the conversation; an interrupted or mismatched attempt needs attention.

**Codex:** its shared server can obscure the terminal that owns a conversation.
Inside Ghostty, setup adds a small PATH shim that starts interactive Codex with
`--no-daemon`. You still use `codex` normally; existing aliases remain intact.
Model and permission choices are not changed by that shim.

Closing an agent deliberately retires its binding when the shell prompt returns.
Closing a tab is tracked separately. Sleep/wake does not relaunch agents that
are still running. Recovery reloads saved conversations; it does not checkpoint
running tool processes or resubmit interrupted prompts.

## Everyday commands

| Command | Purpose |
| --- | --- |
| `ghostback status` | Inspect captured sessions and recovery outcomes |
| `ghostback doctor` | Check installed tools and local configuration |
| `ghostback retry` | Arm failed sessions for retry at their next prompt |
| `ghostback retry --terminal <uuid>` | Retry one affected terminal |
| `ghostback plan --json` | Inspect exact resume arguments without executing them |
| `ghostback inventory` | Read Ghostty's native terminal metadata |
| `ghostback watch --once` | Run one capture pass for diagnosis |
| `ghostback uninstall` | Remove owned integrations while retaining saved sessions |

## When recovery needs attention

Ghostback leaves a terminal alone when it cannot identify the exact conversation,
the saved directory or executable is missing, another process still owns the
session, or a previous attempt ended ambiguously.

Check `ghostback status`, fix the reported cause, then run `ghostback retry` and
press Enter in the affected terminal. It deliberately avoids automatic retry
loops around sign-in failures or sessions that need user input.

Native agent registries are version-sensitive. An unsupported format is reported
instead of being replaced with a “most recent conversation” guess. Existing
custom shell functions that bypass PATH, alternate configuration roots, and
changes in agent hook trust should be checked with `status` after setup.

## Privacy and platform scope

State stays in `~/Library/Application Support/Ghostback`. It contains terminal
and conversation IDs, directories, supported launch options, installation
backups, and recovery outcomes. Chat transcripts are not copied into that state.
Uninstall keeps recovery data and preserves modified or unowned files.

The first native integration is macOS. The domain logic, SQLite storage, and
state machine are portable; native Linux restoration is not currently supported.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
sh scripts/package.sh
```

Release packaging enforces a **10 MiB executable budget per architecture**.
See [architecture](docs/architecture.md), [user experience](docs/user-experience.md),
and [verification](docs/verification.md) for design boundaries and test evidence.

## License

[MIT](LICENSE). Ghostback is an independent project, unaffiliated with Ghostty
or the supported agent vendors.
