#!/usr/bin/env bash
#
# stamp_version.sh — writes one version into every file that carries one, so a
# release branch has something to review and a tagged commit describes itself.
#
#   .claude-plugin/plugin.json   Claude Code only offers a plugin update when
#                                this string *changes*. A release that left it
#                                alone would move the tag and tell nobody.
#   Cargo.toml                   [workspace.package] version — what the built
#                                binaries report.
#   Cargo.lock                   the jod-* entries, which Cargo derives from
#                                the above. Left behind, `cargo build --locked`
#                                fails on a lockfile CI generated itself.
#
# awk rather than a bare `sed s/version/`: all three files contain many version
# strings and only one of each is this repo's. Deterministic and offline — no
# cargo, no network — so tests/release-version.test.sh can assert it directly.
#
# Usage: stamp_version.sh <vX.Y.Z|X.Y.Z> [repo-root]
set -euo pipefail

VERSION="${1:?usage: stamp_version.sh <version> [repo-root]}"
VERSION="${VERSION#v}"
ROOT="${2:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)}"

# _rewrite <file> <awk-program> — applies the program with `v` bound to the
# version, in place, and only if the file exists.
_rewrite() {
  local file="$1" prog="$2" tmp
  [ -f "$file" ] || return 0
  tmp="$file.stamp.$$"
  awk -v v="$VERSION" "$prog" "$file" >"$tmp"
  mv -- "$tmp" "$file"
}

# The manifest's own "version", not the "version" of anything nested under it —
# first match only, which is the top-level key in this file.
_rewrite "$ROOT/.claude-plugin/plugin.json" '
  !done && /^[[:space:]]*"version"[[:space:]]*:/ {
    comma = /,[[:space:]]*$/ ? "," : ""
    sub(/"version"[[:space:]]*:.*/, "\"version\": \"" v "\"" comma)
    done = 1
  }
  { print }
'

# Only the version inside [workspace.package]. Every dependency below it has a
# `version = "…"` line too, and bumping rusqlite to 0.2.0 would be a long
# afternoon.
_rewrite "$ROOT/Cargo.toml" '
  /^\[/ { insection = ($0 == "[workspace.package]") }
  insection && /^version[[:space:]]*=/ {
    $0 = "version = \"" v "\""
    insection = 0
  }
  { print }
'

# Only the [[package]] blocks this workspace owns. `name` always precedes
# `version` within a block, so the name line arms the rewrite and the version
# line disarms it.
_rewrite "$ROOT/Cargo.lock" '
  /^name[[:space:]]*=[[:space:]]*"jod-/ { ours = 1; print; next }
  ours && /^version[[:space:]]*=/ {
    print "version = \"" v "\""
    ours = 0
    next
  }
  /^\[\[package\]\]/ { ours = 0 }
  { print }
'

printf 'stamped %s\n' "$VERSION"
