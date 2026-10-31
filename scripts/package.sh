#!/bin/sh
set -eu

# Run from the checkout root. Each archive contains one native architecture.
# Build dependencies are needed here; end users need only the executable.
host=$(rustc -vV | sed -n 's/^host: //p')
if [ -z "$host" ]; then
    printf 'Could not determine native Rust architecture.\n' >&2
    exit 1
fi
cargo build --release --locked --target "$host"
binary="${CARGO_TARGET_DIR:-target}/$host/release/ghostback"
max_bytes=10485760
binary_bytes=$(wc -c < "$binary" | tr -d ' ')
if [ "$binary_bytes" -gt "$max_bytes" ]; then
    printf 'Release rejected: %s bytes exceeds the %s byte budget.\n' "$binary_bytes" "$max_bytes" >&2
    exit 1
fi
version=$("$binary" --version | sed -n 's/^ghostback //p')
if [ -z "$host" ] || [ -z "$version" ]; then
    printf 'Could not determine release architecture or version.\n' >&2
    exit 1
fi
mkdir -p "${CARGO_TARGET_DIR:-target}" dist
staging=$(mktemp -d "${CARGO_TARGET_DIR:-target}/package-$host.XXXXXX")
trap 'rm -rf "$staging"' EXIT HUP INT TERM
cp "$binary" LICENSE README.md CONTRIBUTING.md "$staging/"
cp -R docs "$staging/"
cargo run --locked --example notices -- "$staging/THIRD_PARTY_NOTICES.txt"
archive="dist/ghostback-$version-$host.tar.gz"
tar -czf "$archive" -C "$staging" ghostback LICENSE README.md CONTRIBUTING.md docs THIRD_PARTY_NOTICES.txt
if command -v sha256sum >/dev/null 2>&1; then
    (cd dist && sha256sum "ghostback-$version-$host.tar.gz") > "$archive.sha256"
else
    (cd dist && shasum -a 256 "ghostback-$version-$host.tar.gz") > "$archive.sha256"
fi
printf 'Binary: %s bytes\nArchive: %s (%s bytes)\n' "$binary_bytes" "$archive" "$(wc -c < "$archive" | tr -d ' ')"
