# Using Ghostback

## One-time setup

Install through Homebrew, then enable account integration:

```sh
brew install cristipufu/tap/ghostback
ghostback setup --dry-run
ghostback setup
```

Setup installs a per-account login helper, shell prompt integration, lifecycle
hooks, and Ghostty's native window-restoration setting. Homebrew installations
use the stable `opt/ghostback/bin/ghostback` path; a standalone source or archive
installation copies its executable to `~/.local/bin/ghostback`.
It merges existing configuration and records its own changes for uninstall.
Open a new Ghostty tab to load the shell integration.

Allow macOS Automation access to Ghostty when requested. Codex can also require
one-time trust for its new hook. Run `ghostback status` to confirm capture.
Existing Codex sessions using a shared server may require one fresh launch.

No Python, uv, Rust toolchain, Node runtime, tmux, account, or remote service is
needed to run the packaged executable. The current Homebrew bottle targets Apple
Silicon on macOS 26; other macOS configurations build from source. Source builds
require Rust and a C compiler
for bundled SQLite.

## Everyday use

Open, split, reorder, and close Ghostty tabs normally. Start `claude`, `codex`,
`agy`, or `grok` with their normal commands. Ghostback records exact conversation
and terminal UUIDs, directories, process identities, and supported launch options.
Several conversations in one repository remain distinct.

The background helper captures process-owned metadata. Agent lifecycle hooks
supply immediate identity changes where available. Codex runs through a small
PATH shim inside Ghostty to keep its server attributable to one terminal.
Custom functions that bypass PATH should be checked with `status`.

Exiting an agent returns to the original shell and retires that binding. Closing
a terminal retires its binding after the watcher confirms its disappearance.
A laptop waking from sleep retains live agents; they are not relaunched.

## Reopening Ghostty

1. Ghostty restores its native windows, tabs, splits, and directories.
2. Each shell loads its prompt integration.
3. Ghostback matches that shell's TTY to a saved native terminal UUID.
4. A new, idle shell atomically claims its saved conversation.
5. The agent starts with its exact resume ID and supported options.
6. A lifecycle hook or fresh owned-process observation confirms the conversation.

The normal flow needs no per-tab `/resume`, conversation picker, or restore
button. Ghostback never pastes commands into your terminals. Recovery reloads
conversation history; it does not checkpoint running tools, preserve network
connections through reboot, or automatically resubmit interrupted prompts.

## Resolving a failure

Run `ghostback status`. It shows the phase, agent, conversation UUID, terminal
UUID, directory, and reason for an unresolved binding. `ghostback doctor` reports
tool paths, Ghostty version, installation state, and whether the recorded helper
process is still alive.

Restore a missing directory or agent executable, resolve sign-in, or repair the
reported configuration. Then run:

```sh
ghostback retry
# Or: ghostback retry --terminal <native-terminal-uuid>
```

Press Enter in each affected terminal. Retry is armed for the next shell prompt;
it does not interrupt a running program. Retired sessions are not rearmed by a
blanket retry. An interrupted claim needs this explicit step rather than an
unbounded automatic retry loop.

A missing or ambiguous conversation identity is retained as a diagnostic. A newer
conversation is never substituted. If an executable moved, rerun setup and check
its path; launch the agent manually once if its old binding still points at a
removed executable.

## Configuration and removal

Setup supports zsh and bash, `ZDOTDIR` within the account home, Ghostty's normal
macOS/XDG config locations, and `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, and `GROK_HOME`.
Use absolute configuration-root paths. Fish, remote SSH hosts, and native Linux
recovery are outside this release's support scope.

`ghostback uninstall` removes managed shell blocks, hooks, login integration,
and unchanged owned executable files. It keeps recovery state and modified or
unowned files. A Homebrew-owned executable is never deleted by this command.
Run `brew uninstall ghostback` afterward to remove the package. Open a new shell afterward to discard functions already loaded
into existing shell processes.

For upgrades, run `brew update` and `brew upgrade ghostback`. Integrations keep
the stable executable path, and the watcher replaces itself when the Homebrew
link moves to a new version. Existing agents continue running.

## Release acceptance scenario

Use a disposable account or VM for destructive qualification:

1. Start eight mixed-agent conversations, including several of one agent in the
   same repository, plus an ordinary shell and a split terminal.
2. Switch a conversation inside each agent; confirm `status` changes its exact ID.
3. Exit one agent deliberately, close one tab, and reorder the others.
4. Quit/reopen Ghostty, then repeat with an actual macOS restart and sleep/wake.
5. Compare every restored terminal UUID and conversation UUID. Require zero
   duplicate launches and no unintended resurrection.
6. Repeat with delayed shell startup, a missing worktree, a moved CLI, expired
   authentication, Automation denial, and a helper crash during recovery.
7. Repair failed cases and retry. Successful terminals must not launch again.
8. Install/uninstall over existing hooks and shell customizations, then verify
   the packaged, signed build under a clean login environment.

The automated native fixture uses its own Ghostty window, generated shell
integration, and fake agents. It verifies command launch and acknowledgement
without sending input to existing user terminals. It does not replace the
real reboot and authenticated-agent release matrix.
