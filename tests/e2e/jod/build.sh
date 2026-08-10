#!/usr/bin/env bash
# Build the three binaries under test from a *clean export of HEAD*, and stage
# them in /tmp/jod-e2e/bin.
#
# Not from the working tree, and not from `target/release`: this checkout is
# shared with several agents who edit and `cargo clean` under it. A suite that
# reads binaries out of `target/` reports whatever half-state the tree was in
# when it happened to run, which is not a test result.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SRC=/tmp/jod-build
BIN=/tmp/jod-e2e/bin

cd "$REPO"
COMMIT="$(git rev-parse HEAD)"

rm -rf "$SRC"
mkdir -p "$SRC" "$BIN"
git archive HEAD | tar -x -C "$SRC"

cd "$SRC"
cargo build --release -p jod-cli -p jod-supervisor -p jod-api

cp target/release/jod target/release/jod-run target/release/jod-api "$BIN/"
echo "$COMMIT" > "$BIN/COMMIT"

echo "staged in $BIN from $COMMIT"
"$BIN/jod" --version
