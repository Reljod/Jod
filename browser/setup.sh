#!/usr/bin/env bash
#
# setup.sh — install the browser Jod's agents use, into its own virtualenv.
#
#   browser/setup.sh                     # install, then prove it works
#   browser/setup.sh --check             # only prove it; change nothing
#
# Why a virtualenv and not `pip install --user`: camoufox pins a patched
# Firefox build and pulls playwright with it. That is not a dependency to put
# in a system Python that other things on the box share. `core/src/paths.rs`
# looks for ~/.jod/browser-venv/bin/python first and falls back to `python3`,
# so a hand-managed environment keeps working if you prefer one.
#
# The proxy is configured separately, in ~/.jod/browser.env — this script does
# not ask for credentials and never writes them. Without it the browser still
# works; it just egresses from this machine's own IP, which is the thing the
# proxy was bought to avoid.
#
#   JOD_PROXY_SERVER    http://p.webshare.io:80
#   JOD_PROXY_USERNAME
#   JOD_PROXY_PASSWORD
#   JOD_PROXY_GEOIP     1 by default; 0 disables
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JOD_HOME="${JOD_HOME:-$HOME/.jod}"
VENV="$JOD_HOME/browser-venv"
ENV_FILE="$JOD_HOME/browser.env"

info() { printf '→ %s\n' "$*"; }
ok()   { printf '✓ %s\n' "$*"; }
warn() { printf '! %s\n' "$*" >&2; }
err()  { printf 'error: %s\n' "$*" >&2; exit 1; }

CHECK_ONLY=0
[ "${1:-}" = "--check" ] && CHECK_ONLY=1

PY="$VENV/bin/python"

if [ "$CHECK_ONLY" -eq 0 ]; then
  command -v python3 >/dev/null 2>&1 || err "python3 is required but not on PATH"

  if [ ! -x "$PY" ]; then
    info "creating a virtualenv at $VENV"
    mkdir -p "$JOD_HOME"
    python3 -m venv "$VENV"
  fi

  info "installing camoufox (this pulls a patched Firefox — expect a few hundred MB)"
  "$PY" -m pip install --quiet --upgrade pip
  "$PY" -m pip install --quiet "camoufox[geoip]"

  # Separate from the pip install and slow: it downloads the browser itself.
  # Skipped when it is already there, which is what makes this safe to re-run.
  info "fetching the Firefox build"
  "$PY" -m camoufox fetch
  ok "camoufox installed"
fi

[ -x "$PY" ] || err "no interpreter at $PY — run this without --check first"

# --- prove it, rather than assume it ---------------------------------------

info "checking the MCP server loads"
"$PY" "$HERE/jod_browser_mcp.py" --selftest || err "the MCP server would not start"

info "checking the protocol answers"
"$PY" "$HERE/test_jod_browser_mcp.py" >/dev/null || err "the protocol tests failed"
ok "protocol and tool surface"

if [ ! -f "$ENV_FILE" ]; then
  warn "no $ENV_FILE — the browser will egress from this machine's own IP."
  warn "Write one with JOD_PROXY_SERVER/USERNAME/PASSWORD to route through Webshare."
else
  ok "proxy configured in $ENV_FILE"
fi

# The one check that cannot be faked: what IP the world actually sees. A
# configured proxy and a working one are different facts, and only the second
# is worth anything.
info "checking real egress (launches the browser; needs network)"
if JOD_BROWSER_DIR="$HERE" "$PY" - <<'PY'
import sys, os
sys.path.insert(0, os.environ["JOD_BROWSER_DIR"])
from jod_browser_mcp import CamoufoxSession, tool_status
s = CamoufoxSession()
try:
    print(tool_status(s, {}))
finally:
    s.close()
PY
then
  ok "egress checked above — confirm the IP is the proxy's, not this box's"
else
  warn "egress check failed. The server is installed and the protocol works,"
  warn "but no page was actually fetched. Fix this before trusting a scrape."
  exit 1
fi
