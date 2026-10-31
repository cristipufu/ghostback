# Contributing to Ghostback

The product is deliberately small: exact session recovery in stock Ghostty.
Changes should make that workflow more reliable without adding a terminal UI,
cloud service, or large runtime.

## Local checks

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
node --test tests/bridges.mjs
sh scripts/package.sh
```

Node is used only for bridge development tests and by agents that already ship
a JavaScript runtime; the Ghostback executable has no runtime dependency.

Keep `Cargo.lock` current. The package script rejects binaries over 10 MiB per
architecture. Test fixtures must use temporary homes and state directories.
Never install hooks into the contributor's account, type into existing user
terminals, or terminate existing agents from an automated test.

## What a useful change includes

- A clear trigger and expected outcome.
- A regression test for a fixed failure, especially identity or concurrency bugs.
- Accurate compatibility notes when depending on an agent's internal registry.
- Evidence for native integration changes. A fixture is useful, but is not a
  substitute for testing the actual macOS path.

Keep process inspection in `native`, agent protocols in `capture`, argument
policy in `agents`, and recovery decisions in `lifecycle`. Application workflows
compose those modules. Avoid introducing interfaces or layers until more than
one implementation or a specific testability need justifies them.

The correctness rule is simple: recover the exact conversation in the exact
terminal, or leave the terminal alone and explain why recovery was skipped.
