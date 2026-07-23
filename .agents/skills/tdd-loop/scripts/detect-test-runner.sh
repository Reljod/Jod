#!/usr/bin/env bash
# Print the default test command for the current repo, one line, to stdout.
# Exits non-zero and prints nothing if no runner can be inferred.
# Detection is best-effort and ordered by specificity; override by passing
# an explicit command to tdd-loop.sh.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

has() { [ -f "$ROOT/$1" ]; }
pkg_has() { has package.json && grep -q "\"$1\"" "$ROOT/package.json" 2>/dev/null; }

# --- JavaScript / TypeScript ---
if has package.json; then
  if pkg_has vitest; then echo "npx vitest run"; exit 0; fi
  if pkg_has jest; then echo "npx jest"; exit 0; fi
  if grep -q '"test"' "$ROOT/package.json" 2>/dev/null; then echo "npm test --silent"; exit 0; fi
fi

# --- Python ---
if has pyproject.toml || has pytest.ini || has tox.ini || has setup.cfg; then
  if grep -rqs "pytest" "$ROOT/pyproject.toml" "$ROOT/pytest.ini" "$ROOT/setup.cfg" 2>/dev/null; then
    echo "pytest"; exit 0
  fi
fi
if has pyproject.toml || [ -d "$ROOT/tests" ]; then echo "pytest"; exit 0; fi

# --- Go ---
if has go.mod; then echo "go test ./..."; exit 0; fi

# --- Rust ---
if has Cargo.toml; then echo "cargo test"; exit 0; fi

# --- Ruby ---
if has Gemfile && { [ -d "$ROOT/spec" ] || grep -qs rspec "$ROOT/Gemfile" 2>/dev/null; }; then
  echo "bundle exec rspec"; exit 0
fi
if has Rakefile; then echo "rake test"; exit 0; fi

# --- JVM ---
if has pom.xml; then echo "mvn -q test"; exit 0; fi
if has build.gradle || has build.gradle.kts; then echo "./gradlew test"; exit 0; fi

# --- Elixir ---
if has mix.exs; then echo "mix test"; exit 0; fi

echo "could not detect a test runner; pass an explicit command to tdd-loop.sh" >&2
exit 1
