#!/bin/sh
set -eu
archive=${1:?usage: sh scripts/smoke-package.sh dist/ghostback-version-host.tar.gz}
smoke_root=$(mktemp -d "${TMPDIR:-/tmp}/ghostback-smoke.XXXXXX")
trap 'rm -rf "$smoke_root"' EXIT HUP INT TERM
tar -xzf "$archive" -C "$smoke_root"
test -s "$smoke_root/LICENSE"
test -s "$smoke_root/THIRD_PARTY_NOTICES.txt"
test -s "$smoke_root/docs/verification.md"
mkdir "$smoke_root/home"
env -i HOME="$smoke_root/home" PATH=/usr/bin:/bin "$smoke_root/ghostback" --version
env -i HOME="$smoke_root/home" PATH=/usr/bin:/bin "$smoke_root/ghostback" --state-dir "$smoke_root/state" doctor > "$smoke_root/doctor.json"
env -i HOME="$smoke_root/home" PATH=/usr/bin:/bin "$smoke_root/ghostback" --state-dir "$smoke_root/state" setup --dry-run --no-launch > "$smoke_root/setup-plan.txt"
test ! -e "$smoke_root/state"
printf 'PASS: packaged executable runs without a development runtime.\n'
