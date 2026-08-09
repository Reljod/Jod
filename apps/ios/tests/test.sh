#!/usr/bin/env bash
#
# The iOS client's gate, in the shape `.github/workflows/tests.yml` discovers
# (`*/tests/test.sh`). Without this the suite would only ever run on the machine
# of whoever remembered to type `npm test`, and the charter's rule is that
# "tested" means CI ran it.
#
# It typechecks and runs the unit suite. It does **not** build the iOS binary:
# that needs Xcode, which no Linux runner has. What the app does — the reducer,
# the transport, the conversation rules — is all platform-free on purpose, so
# the part a runner cannot reach is the shell around it, not the behaviour.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$here"

if ! command -v node >/dev/null 2>&1; then
  echo "FAIL: node is required to test the iOS client (needs >= 20)" >&2
  exit 1
fi

major="$(node -p 'process.versions.node.split(".")[0]')"
if [ "$major" -lt 20 ]; then
  echo "FAIL: node $major is too old; the iOS client needs >= 20" >&2
  exit 1
fi

# `npm ci` is the reproducible install and needs the lockfile; fall back only
# when it is genuinely absent, never to paper over an install that failed.
if [ -f package-lock.json ]; then
  npm ci --no-audit --no-fund >/dev/null
else
  npm install --no-audit --no-fund >/dev/null
fi

echo "--- typecheck"
npx --no-install tsc --noEmit

echo "--- unit tests"
npx --no-install vitest run

echo "PASS: apps/ios"
