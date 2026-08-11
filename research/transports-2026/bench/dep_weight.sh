#!/usr/bin/env bash
# What each transport option actually costs Jod's dependency tree.
#
# "Small" and "heavy" are opinions; the number of crates that would be added to
# Cargo.lock is not. For each candidate this builds a throwaway crate, resolves
# it, and counts the packages that are NOT already in Jod's lockfile — the
# marginal cost, which is the only figure that matters here.
#
#   research/transports-2026/bench/dep_weight.sh
#
# Each candidate is a list of `crate@version[:feature,feature][:--no-default-features]`
# tokens, added one `cargo add` at a time so that features land on the right crate.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
lock="$repo_root/Cargo.lock"
[ -f "$lock" ] || { echo "no Cargo.lock at $lock" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

grep -E '^name = ' "$lock" | sed 's/name = "//; s/"$//' | sort -u > "$work/have.txt"
echo "Jod's Cargo.lock already contains $(wc -l < "$work/have.txt") packages."
printf '\n%-34s %8s %8s   %s\n' "candidate" "total" "new" "new crates (first 8)"
printf -- '-%.0s' {1..100}; echo

try() {
  local label="$1"; shift
  local slug; slug="$(echo "$label" | tr -c 'a-zA-Z0-9' '_')"
  local dir="$work/$slug"
  cargo new --quiet --lib "$dir" >/dev/null 2>&1
  printf '\n[workspace]\n' >> "$dir/Cargo.toml"

  local token spec feats nodefault
  for token in "$@"; do
    spec="${token%%:*}"
    feats=""; nodefault=""
    case "$token" in
      *:nodefault:*) nodefault="--no-default-features"; feats="${token##*:}" ;;
      *:nodefault)   nodefault="--no-default-features" ;;
      *:*)           feats="${token#*:}" ;;
    esac
    local args=(add --quiet --manifest-path "$dir/Cargo.toml" "$spec")
    [ -n "$nodefault" ] && args+=("$nodefault")
    [ -n "$feats" ] && args+=(--features "$feats")
    if ! cargo "${args[@]}" >"$work/$slug.err" 2>&1; then
      printf '%-34s %8s %8s   %s\n' "$label" "FAILED" "-" "$(tail -1 "$work/$slug.err")"
      return
    fi
  done

  if ! cargo tree --quiet --manifest-path "$dir/Cargo.toml" --prefix none --edges normal \
        > "$work/$slug.tree" 2>"$work/$slug.err"; then
    printf '%-34s %8s %8s   %s\n' "$label" "FAILED" "-" "$(tail -1 "$work/$slug.err")"
    return
  fi
  # Drop the throwaway root package itself; it is not a dependency of anything.
  awk '{print $1}' < "$work/$slug.tree" | grep -v '^$' | grep -v "^$slug\$" | sort -u > "$work/$slug.txt"

  local total new sample
  total=$(wc -l < "$work/$slug.txt")
  comm -23 "$work/$slug.txt" "$work/have.txt" > "$work/$slug.new"
  new=$(wc -l < "$work/$slug.new")
  sample=$(head -8 "$work/$slug.new" | tr '\n' ' ')
  printf '%-34s %8s %8s   %s\n' "$label" "$total" "$new" "$sample"
}

# The GitHub webhook receiver.
try "hmac (github signatures)" "hmac@0.12"

# Telegram, smallest plausible to largest.
try "hyper+rustls (raw HTTP)" "hyper@1:client,http1" "hyper-rustls@0.27" "hyper-util@0.1:client-legacy,tokio" "http-body-util@0.1"
# The same thing with rustls' `ring` provider and compiled-in roots instead of
# aws-lc-rs + the platform trust store. `aws-lc-sys` needs a C toolchain and
# cmake at build time, which is a real cost on a small VPS.
try "hyper+rustls/ring+webpki-roots" "hyper@1:client,http1" "hyper-rustls@0.27:nodefault:http1,ring,tls12,webpki-tokio" "hyper-util@0.1:client-legacy,tokio" "http-body-util@0.1"
# reqwest 0.13 renamed the TLS features: `rustls` is the one, and `default`
# already implies it. The minimal form drops http2, charset and system-proxy,
# none of which the Telegram Bot API needs.
try "reqwest minimal (raw HTTP)" "reqwest@0.13:nodefault:rustls,json"
try "reqwest default (raw HTTP)" "reqwest@0.13:json"
try "ureq (blocking raw HTTP)" "ureq@3:json"
try "frankenstein (reqwest)" "frankenstein@0.50:nodefault:client-reqwest"
try "teloxide (default/native-tls)" "teloxide@0.17"
try "teloxide (rustls)" "teloxide@0.17:nodefault:rustls,ctrlc_handler"

echo
echo "'new' is what would be added to Cargo.lock. Lower is not automatically"
echo "better, but for a box that also runs agent harnesses it is the axis that"
echo "shows up in build time, audit surface and supply-chain exposure."
