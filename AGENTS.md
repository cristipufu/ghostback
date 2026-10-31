# Working on Ghostback

- The project is Rust. Use Cargo and commit `Cargo.lock` when dependencies change.
  End users must not need Python, uv, Rust, Cargo, or a separate runtime.
- Check with `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test --locked`. Build with `cargo build --release --locked` and measure
  the executable. Keep each architecture's uncompressed release under 10 MiB.
- Read `docs/user-experience.md` and `docs/architecture.md` before extending
  session capture or restoration. Keep shipped behavior distinct from plans.
- Keep the session model and storage portable. Put platform automation in
  terminal adapters. Use exact conversation IDs and native terminal identities.
- Never infer a conversation from its directory, title, or most recent file.
  Ghostty's core hex surface ID is different from its persistent AppleScript UUID.
- Exercise concurrency and crash recovery with temporary state. Tests must not
  send input into the user's terminals or terminate the user's processes.
- Failures must preserve saved recovery data and explain the skipped action.
  Command delivery is not acknowledgement that the intended session resumed.
