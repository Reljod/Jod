#!/usr/bin/env bash
# Runs a command with OPENROUTER_API_KEY injected from Doppler.
#
# The Doppler service token for `jod-apps` is *scoped to the Jod-Apps directory*
# (see `doppler configure`), and this workplace's user auth is currently
# disabled — so `doppler run --project jod-apps` fails while a plain
# `doppler run` from that directory succeeds. We therefore resolve config there
# and hop back here, rather than copying the token into this repo.
#
#   ./scripts/with-key.sh node scripts/bench.mjs
#
# Override the scope directory with JOD_APPS_DIR if your checkout differs.
set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JOD_APPS_DIR="${JOD_APPS_DIR:-$HOME/Developer/Repositories/Projects/Jod-Apps}"

if [ $# -eq 0 ]; then
  echo "usage: $0 <command> [args...]" >&2
  exit 64
fi

if ! command -v doppler >/dev/null 2>&1; then
  echo "doppler CLI not found. Install it, or export OPENROUTER_API_KEY yourself." >&2
  exit 69
fi

if [ ! -d "$JOD_APPS_DIR" ]; then
  echo "Doppler scope directory not found: $JOD_APPS_DIR" >&2
  echo "Set JOD_APPS_DIR to the checkout that holds the jod-apps Doppler scope." >&2
  exit 66
fi

cd "$JOD_APPS_DIR"
# `doppler run` reads its config from the current directory, then we return to
# the app directory to actually execute. "$@" is re-quoted through a helper so
# arguments with spaces survive the hop.
exec doppler run -- env JOD_VOICE_APP_DIR="$APP_DIR" bash -c 'cd "$JOD_VOICE_APP_DIR" && exec "$@"' _ "$@"
