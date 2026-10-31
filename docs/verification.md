# Verification

Evidence collected on October 31, 2026, on Apple Silicon macOS with Ghostty
1.4.0. The macOS workflow is implemented. Release qualification is tracked
separately from implementation; no claim of exhaustive crash-proof behavior is
made.

## Automated checks

- **87 Rust tests pass** across session storage, adapters, installation, and
  lifecycle behavior.
- Formatting and strict Clippy checks pass for all targets in the checkout,
  including the test and release-tool sources.
- RustSec `cargo-audit` 0.22.2 scanned 69 locked dependencies against 1,395
  advisories with warnings denied: no reported vulnerabilities or warnings.
- The package contains a standalone native executable, MIT license, README,
  contributor guide, documentation, third-party notices for 42 normal/build
  dependencies, and a SHA-256 checksum sidecar.
- The Apple Silicon executable measures approximately **2.3 MB**, below the
  enforced **10 MiB** limit. SQLite is compiled in. The dynamic dependencies are
  macOS system libraries (`libSystem` and `libiconv`).
- The extracted package runs with an empty environment plus HOME and system
  PATH; no Python, uv, Node, Cargo, or Rust toolchain is available on that PATH.
  Diagnostics leave absent state untouched.

The repository runs macOS/Linux CI checks, a macOS packaging smoke check,
and RustSec auditing on GitHub. The initial published build passed all jobs;
subsequent results are available in the [CI workflow](https://github.com/cristipufu/ghostback/actions/workflows/ci.yml).

The dependency refresh uses the latest stable direct releases and refreshes
compatible transitive packages. A standard SHA-256 known-answer test verifies
that the `sha2` 0.11 migration preserves existing installer ownership fingerprints.

## Native integration evidence

Read-only capture against the running workspace resolved five exact bindings:
two Claude Code, two agy, and one Grok Build. Native inventory enumerated eight
terminals and distinguished tabs sharing a directory. Installed executables
included Claude Code 2.1.316, Codex 0.166.0, and Grok Build 1.0.70.

Two pre-existing shared-server Codex sessions could not be attributed safely.
Their diagnostic state reports unavailable or ambiguous identity. No directory,
recent transcript, or title-based guess was substituted. The installed Codex
shim uses a dedicated server for new interactive sessions.

The opt-in native fixture creates and closes only its own Ghostty window. It
uses a saved binding, an original shell that exits, a new interactive shell,
the generated shell integration, and a fake local agent. These cases passed:

| Agent fixture | Shell | Acknowledgement |
| --- | --- | --- |
| Claude | zsh | Exact lifecycle hook |
| Codex | zsh | Exact lifecycle hook; dedicated-server flag retained |
| agy | zsh | Native conversation/workspace hook payload |
| Grok | zsh | Registry matching PID and process birth time |
| Claude | bash | Exact lifecycle hook |

Each fixture verified the native terminal UUID, automatic prompt invocation,
exact resume arguments, conversation acknowledgement, and retirement on normal
exit. It sent no input to existing user terminals and launched no authenticated
agent or model request.

To reproduce with Ghostty already running:

```sh
cargo build --release --locked --bin ghostback --example native_e2e
./target/release/examples/native_e2e ./target/release/ghostback claude
./target/release/examples/native_e2e ./target/release/ghostback codex
./target/release/examples/native_e2e ./target/release/ghostback agy
./target/release/examples/native_e2e ./target/release/ghostback grok
./target/release/examples/native_e2e ./target/release/ghostback claude bash
```

A separate fixture bootstrapped a uniquely named temporary launchd job using
the generated helper plist. The background helper started and completed native
capture, then the fixture removed its own job. Existing startup configuration
and terminal contents were untouched. Reproduce with:

```sh
cargo build --release --locked --example launch_agent_e2e
./target/release/examples/launch_agent_e2e ./target/release/ghostback
```

Ghostty 1.4.0 source inspection confirms persistence of its native UUID. The
fixture replaces the shell in a live terminal; it does not quit Ghostty or
restart macOS.

## Failure cases exercised

| Area | Checks |
| --- | --- |
| Identity | Same-directory conversations, UUID casing, switched sessions, stale registries, PID birth checks, ambiguous TTY mappings, duplicate IDs |
| Restore lifecycle | One exclusive claim under concurrency, interrupted claims, failed attempts without retry loops, mismatched acknowledgement preserving the saved ID |
| Deliberate exits | Original-shell retirement, delayed observations cannot revive a retired process, closed-terminal confirmation, Ghostty generation changes |
| Argument handling | No initial prompt replay, boolean/value flag distinctions, explicit resume IDs, tampered saved options rejected, Codex shim recursion refused, child exit status retained |
| Persistence | Concurrent writers in threads and processes, abrupt child exit mid-transaction, corrupt database retention, future schema refusal, late exit cannot erase a new conversation |
| Adapter parsing | Registry ambiguity, process argument/environment separation, malformed bytes, deterministic random parser inputs, agy clear/switch/partial-write/truncation/rotation cases |
| Setup | Dry-run without writes, idempotence, hook merging, malformed configuration preflight, unowned executable protection, symlink refusal, shell syntax and existing prompt preservation |
| Setup recovery | Rollback after incomplete writes, committed journal recognition, retained concurrent user edits, original executable mode restoration, invalid backup path refusal |
| Uninstall | Managed blocks removed, user hooks and additions retained, modified owned executable preserved, malformed configuration blocks partial removal |
| Subprocesses | Deadlines, bounded output, inherited pipes held open by descendants, owned-child cleanup, downstream broken pipes |
| Paths/input | Missing directories retained, invalid/non-UTF-8 paths diagnosed, quoted paths preserved, oversized/malformed hook payload rejection |

## Release qualification still required

These are external integration and distribution checks, not missing automatic
capture, installer, or restore implementations:

- Quit/reopen Ghostty and restart macOS in a disposable account or VM; compare
  native UUIDs and exact conversations across a mixed-agent workspace.
- Resume real authenticated sessions for every supported agent version,
  including in-agent conversation switches, expired authentication, missing
  worktrees, and CLI upgrades.
- Exercise login-helper startup and macOS Automation permission grant, denial,
  and revocation from a clean account with delayed shell initialization.
- Sign/notarize the distributed macOS executable and validate quarantine and
  clean-machine installation on each architecture advertised for release.

The complete acceptance scenario is in
[user-experience.md](user-experience.md#release-acceptance-scenario).
Native Linux recovery, fish integration, and remote SSH recovery are outside the
current macOS release scope.
