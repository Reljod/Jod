#!/usr/bin/env bash
#
# plugin.test.sh — validates the Claude Code plugin this repo ships from its
# own root: `.claude-plugin/plugin.json` (the manifest) and
# `.claude-plugin/marketplace.json` (the catalog that serves it).
#
# Why a suite and not just `claude plugin validate`: that command needs the
# `claude` CLI, which CI and a fresh box don't have — and, more to the point,
# it passed on two manifests that were badly broken at *install* time. It
# checks shape, not loading. What actually breaks this plugin:
#
#   1. Declaring `agents` or `hooks` in the manifest. Both validate clean and
#      then fail: `agents` loads nothing at all, `hooks` double-loads the
#      standard file and takes the whole plugin down. Section 2 fails if
#      either key comes back.
#   2. The manifest points `skills` at `.agents/skills/` instead of copying it,
#      so a renamed or moved directory silently ships a plugin with no skills.
#      Every declared path is asserted to resolve.
#   3. A plugin's skills run from `~/.claude/plugins/cache/...`, not from the
#      user's repo, so any script path written relative to the *repo* is dead
#      on arrival. Skills reference bundled scripts via ${CLAUDE_SKILL_DIR};
#      section 6 fails if a repo-relative invocation comes back, and checks
#      that every substituted path names a file that exists.
#   4. Agents ship from `agents/` but the repo also needs them in
#      `.claude/agents/` to work here without the plugin. Symlinks aren't
#      followed by the plugin loader, so both are real copies and section 2
#      fails the moment they drift apart.
#
# Deliberately offline and read-only — no install, no network, no `claude`.
# Run: tests/plugin.test.sh
set -u

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$REPO_ROOT/.agents/skills/test-scenarios/scripts/assert.sh"
cd "$REPO_ROOT" || exit 1

MANIFEST=".claude-plugin/plugin.json"
MARKET=".claude-plugin/marketplace.json"

command -v python3 >/dev/null 2>&1 || {
  echo "plugin.test.sh needs python3 to parse JSON — not found on PATH" >&2
  exit 1
}

echo "== Claude plugin test suite =="

# JSON readers. python3 rather than jq: jq is not preinstalled everywhere, and
# a missing parser must fail the suite loudly rather than skip these checks.
json_str() {  # file key[.key...]  -> scalar, empty if absent
  python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
for k in sys.argv[2].split("."):
    if not isinstance(d,dict): d=""; break
    d=d.get(k,"")
print(d if isinstance(d,str) else "")' "$1" "$2"
}
json_list() {  # file key -> one item per line, tolerating string-or-array
  python3 -c '
import json,sys
v=json.load(open(sys.argv[1])).get(sys.argv[2]) or []
print("\n".join([v] if isinstance(v,str) else v))' "$1" "$2"
}

# --- 1. the manifest is loadable and identifies the plugin ------------------
section "plugin manifest"
assert_file "$MANIFEST"
assert_ok python3 -c "import json;json.load(open('$MANIFEST'))"
assert_eq "$(json_str "$MANIFEST" name)" "jod" "plugin name is 'jod' (the /jod:… namespace)"
ok "[[ '$(json_str "$MANIFEST" name)' =~ ^[a-z0-9]+(-[a-z0-9]+)*$ ]]" "plugin name is kebab-case"
# An explicit version pins updates to a deliberate bump; without it every
# commit reads as a new version to everyone who installed the plugin.
ok "[[ '$(json_str "$MANIFEST" version)' =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]" "version is semver"
# Asserted by length, not by value: `ok` evals its expression, and prose
# containing an apostrophe (Reljod's) would break the quoting.
m_desc="$(json_str "$MANIFEST" description)"
m_author="$(json_str "$MANIFEST" author.name)"
ok "[ ${#m_desc} -ge 40 ]" "description is substantive (${#m_desc} chars, shown in /plugin)"
ok "[ ${#m_author} -ge 1 ]" "author.name is set"

# The documented #1 plugin mistake: component dirs nested inside
# .claude-plugin/, where Claude Code never scans them.
for d in skills commands agents hooks; do
  assert_missing ".claude-plugin/$d" "no $d/ nested inside .claude-plugin/"
done

# --- 2. every declared component path resolves ------------------------------
# The whole point of pointing the manifest at the existing trees is that there
# is no second copy to drift; the cost is that a move breaks the plugin
# silently, so each declared path is checked.
section "declared component paths resolve"
skill_roots=()
while IFS= read -r p; do
  [ -n "$p" ] || continue
  ok "[[ '$p' == ./* ]]" "skills path is plugin-root-relative: $p"
  assert_dir "${p#./}" "skills path exists: $p"
  skill_roots+=("${p#./}")
done < <(json_list "$MANIFEST" skills)
ok "[ ${#skill_roots[@]} -gt 0 ]" "manifest declares at least one skills path"

# Agents and hooks load ONLY from their default locations. Both manifest keys
# are actively harmful here, and each failed differently against Claude Code
# 2.1.220 — `claude plugin validate` passes on both, so only an install shows it:
#
#   "agents": [...]  -> validates, then loads nothing. Agents (0). A directory
#                       value is rejected outright ("agents: Invalid input"); an
#                       explicit .md file list validates and still loads nothing.
#   "hooks": "./hooks/hooks.json"
#                    -> "Duplicate hooks file detected", plugin fails to load
#                       entirely, because hooks/hooks.json is auto-discovered.
#
# So the manifest must stay silent about both and let discovery do it.
for key in agents hooks; do
  declared="$(json_list "$MANIFEST" "$key")"
  n="$(printf '%s' "$declared" | grep -c . || true)"
  ok "[ ${n:-0} -eq 0 ]" "manifest does not declare '$key' (breaks loading; discovery handles it)"
done

# Agents live in agents/ at the plugin root — the only place a plugin reads
# them from. .claude/agents/ is NOT read for plugins even when named explicitly.
AGENT_DIR="agents"
PROJECT_AGENT_DIR=".claude/agents"
assert_dir "$AGENT_DIR" "agents/ exists at the plugin root"
agent_files=()
while IFS= read -r f; do
  [ -n "$f" ] || continue
  agent_files+=("$f")
done < <(find "$AGENT_DIR" -maxdepth 1 -name '*.md' | sort)
ok "[ ${#agent_files[@]} -gt 0 ]" "agents/ holds at least one agent"

# The repo needs the same agents in .claude/agents/ so they work here without
# the plugin installed, and symlinks are not followed by the plugin loader —
# so these are two real copies and this is the gate that keeps them identical.
while IFS= read -r f; do
  [ -n "$f" ] || continue
  name="$(basename "$f")"
  twin="$PROJECT_AGENT_DIR/$name"
  assert_file "$twin" "${name%.md}: present in $PROJECT_AGENT_DIR too"
  [ -f "$twin" ] || continue
  if diff -q "$f" "$twin" >/dev/null 2>&1; then
    pass "${name%.md}: agents/ and $PROJECT_AGENT_DIR/ copies are identical"
  else
    fail "${name%.md}: agents/ and $PROJECT_AGENT_DIR/ have DRIFTED"
  fi
done < <(printf '%s\n' "${agent_files[@]}")
# ...and the reverse, so an agent added only on the project side still ships.
while IFS= read -r f; do
  [ -n "$f" ] || continue
  assert_file "$AGENT_DIR/$(basename "$f")" \
    "$(basename "$f" .md): present in $AGENT_DIR/ too (or it never ships)"
done < <(find "$PROJECT_AGENT_DIR" -maxdepth 1 -name '*.md' | sort)

# Fixed, not read from the manifest: this is the auto-discovered location, and
# naming it in the manifest is what breaks the plugin (see above).
HOOKS_PATH="hooks/hooks.json"
assert_file "$HOOKS_PATH" "hooks config exists at the discovered path: $HOOKS_PATH"

# --- 3. every skill is loadable as a skill ----------------------------------
# A directory under a declared skills path with no SKILL.md, or whose
# frontmatter name disagrees with its directory, does not load as /jod:<name>.
section "skills load"
skill_count=0
for root in "${skill_roots[@]}"; do
  for dir in "$root"*/; do
    [ -d "$dir" ] || continue
    name="$(basename "$dir")"
    sk="$dir/SKILL.md"
    assert_file "$sk" "$name has a SKILL.md"
    [ -f "$sk" ] || continue
    skill_count=$((skill_count + 1))
    # Frontmatter = the lines between the opening --- and the next ---.
    fm="$(awk 'NR==1 && $0!="---"{exit} NR==1{next} $0=="---"{exit} {print}' "$sk")"
    assert_eq "$(printf '%s\n' "$fm" | sed -n 's/^name:[[:space:]]*//p' | head -n1)" \
      "$name" "$name: frontmatter name matches its directory"
    # Folded (`description: >`) descriptions carry the text on the following
    # indented lines, so measure the whole block, not the key's own line.
    desc="$(printf '%s\n' "$fm" | awk '
      /^description:/ {f=1; sub(/^description:[[:space:]]*/,""); if($0!=">"&&$0!="|") printf "%s ",$0; next}
      f && /^[[:space:]]+[^[:space:]]/ {printf "%s ",$0; next}
      f {exit}')"
    ok "[ ${#desc} -ge 40 ]" "$name: description is substantive (${#desc} chars)"
  done
done
ok "[ $skill_count -ge 5 ]" "all $skill_count skills load"

# --- 4. every agent is loadable as a subagent -------------------------------
section "agents load"
agent_count=0
for f in "${agent_files[@]}"; do
  [ -f "$f" ] || continue
  name="$(basename "$f" .md)"
  agent_count=$((agent_count + 1))
  fm="$(awk 'NR==1 && $0!="---"{exit} NR==1{next} $0=="---"{exit} {print}' "$f")"
  assert_eq "$(printf '%s\n' "$fm" | sed -n 's/^name:[[:space:]]*//p' | head -n1)" \
    "$name" "$name: frontmatter name matches its filename"
  ok "[ -n \"\$(printf '%s\n' \"\$fm\" | sed -n 's/^description:[[:space:]]*//p')\" ]" \
    "$name: has a description"
done
ok "[ $agent_count -ge 4 ]" "all $agent_count agents load"

# --- 5. hooks are wired to a real, runnable script --------------------------
# ${CLAUDE_PLUGIN_ROOT} is mandatory here: a plugin hook runs from the cache
# directory, so a bare relative path finds nothing.
section "hooks"
assert_ok python3 -c "import json;json.load(open('$HOOKS_PATH'))"
# The resolving happens in python, not sed: BSD sed (macOS) has no `\?`, and
# the raw command contains `${CLAUDE_PLUGIN_ROOT}` — which bash would try to
# expand if it ever reached an `eval`. Emits: anchored?<TAB>repo-relative path.
hook_cmds="$(python3 -c '
import json,sys
cfg=json.load(open(sys.argv[1])).get("hooks",{})
for entries in cfg.values():
    for e in entries:
        for h in e.get("hooks",[]):
            cmd=h.get("command")
            if not cmd: continue
            anchored="yes" if "${CLAUDE_PLUGIN_ROOT}" in cmd else "no"
            # The plugin root is this repo, so strip the variable (and the
            # quotes around it) and keep the leading word: the script path.
            p=cmd.replace("\"${CLAUDE_PLUGIN_ROOT}\"","").replace("${CLAUDE_PLUGIN_ROOT}","")
            print("%s\t%s" % (anchored, p.split()[0].lstrip("/")))' "$HOOKS_PATH")"
hook_n="$(printf '%s' "$hook_cmds" | grep -c . || true)"
ok "[ ${hook_n:-0} -ge 1 ]" "hooks config declares at least one command ($hook_n)"
while IFS=$'\t' read -r anchored script; do
  [ -n "$script" ] || continue
  assert_eq "$anchored" "yes" "hook command is plugin-root-anchored: ${script##*/}"
  assert_file "$script" "hook script exists: $script"
  ok "[ -x '$script' ]" "hook script is executable: $script"
done < <(printf '%s\n' "$hook_cmds")

# --- 6. skills are portable out of this repo --------------------------------
# The regression this section exists for: a skill that tells Claude to run
# `.agents/skills/<name>/scripts/x.sh` works in this checkout and fails for
# every plugin user, because their cwd is their own project.
section "skills reference bundled scripts portably"
for root in "${skill_roots[@]}"; do
  for dir in "$root"*/; do
    [ -d "$dir" ] || continue
    name="$(basename "$dir")"
    sk="$dir/SKILL.md"
    [ -f "$sk" ] || continue
    assert_no_grep "$root$name/scripts/" "$sk" \
      "$name: no repo-relative script invocation"
    # Same trap one layer down: a *script* that prints
    # `.agents/skills/<self>/…` sends a plugin user to a path they don't have.
    # tests/ is exempt — those run from a Jod checkout, never from an install,
    # and setup-project legitimately names both the source root it copies from
    # and the target layout it writes.
    if [ "$name" != "setup-project" ]; then
      hits="$(grep -rl "$root$name/" "$dir" 2>/dev/null | grep -v "/tests/" || true)"
      hit_n="$(printf '%s' "$hits" | grep -c . || true)"
      ok "[ ${hit_n:-0} -eq 0 ]" \
        "$name: nothing outside tests/ hardcodes $root$name/ (${hit_n} file(s))"
    fi
    # Every substituted path must name a file that is actually bundled.
    while IFS= read -r ref; do
      [ -n "$ref" ] || continue
      assert_file "$dir$ref" "$name: \${CLAUDE_SKILL_DIR}/$ref is bundled"
    done < <(grep -o '\${CLAUDE_SKILL_DIR}/[A-Za-z0-9_./-]*' "$sk" \
             | sed 's|\${CLAUDE_SKILL_DIR}/||' | sort -u)
  done
done

# A skill reached through its `.claude/commands/<name>.md` wrapper is *read as
# a file*, so Claude Code substitutes nothing — the wrapper has to say what
# ${CLAUDE_SKILL_DIR} means or the copied-into-a-repo install breaks.
section "command wrappers resolve \${CLAUDE_SKILL_DIR}"
for root in "${skill_roots[@]}"; do
  for dir in "$root"*/; do
    name="$(basename "$dir")"
    wrapper=".claude/commands/$name.md"
    [ -f "$dir/SKILL.md" ] && [ -f "$wrapper" ] || continue
    grep -q 'CLAUDE_SKILL_DIR' "$dir/SKILL.md" || continue
    assert_grep 'CLAUDE_SKILL_DIR' "$wrapper" "/$name wrapper explains the variable"
    assert_grep "$root$name" "$wrapper" "/$name wrapper names the skill's directory"
  done
done

# --- 7. the marketplace serves this plugin ----------------------------------
section "marketplace catalog"
assert_file "$MARKET"
assert_ok python3 -c "import json;json.load(open('$MARKET'))"
mk_name="$(json_str "$MARKET" name)"
mk_owner="$(json_str "$MARKET" owner.name)"
ok "[ ${#mk_name} -ge 1 ]" "marketplace has a name ($mk_name)"
ok "[ ${#mk_owner} -ge 1 ]" "marketplace has owner.name"
# Names reserved for Anthropic stop loading as untrusted, so a rename that
# lands on one takes the whole catalog down.
RESERVED="claude-code-marketplace claude-code-plugins claude-plugins-official
claude-plugins-community claude-community anthropic-marketplace
anthropic-plugins agent-skills anthropic-agent-skills knowledge-work-plugins
life-sciences claude-for-legal claude-for-financial-services
financial-services-plugins first-party-plugins healthcare"
assert_no_grep "$(json_str "$MARKET" name)" <(printf '%s\n' $RESERVED) \
  "marketplace name is not reserved for Anthropic"

# Each entry's source must resolve, and the entry serving this repo must agree
# with the manifest — the marketplace entry name is what `/plugin install`
# and `enabledPlugins` key on.
entries="$(python3 -c '
import json,sys
for p in json.load(open(sys.argv[1])).get("plugins",[]):
    s=p.get("source")
    print("%s\t%s" % (p.get("name",""), s if isinstance(s,str) else "<remote>"))' "$MARKET")"
entry_n="$(printf '%s' "$entries" | grep -c . || true)"
ok "[ ${entry_n:-0} -ge 1 ]" "marketplace lists at least one plugin ($entry_n)"
found_self=0
while IFS=$'\t' read -r pname psource; do
  [ -n "$pname" ] || continue
  case "$psource" in
    "<remote>") continue ;;
    # "./" is the marketplace root itself, which strips to the empty string —
    # normalize that to "." so the directory check means what it reads as.
    ./*)
      target="${psource#./}"
      assert_dir "${target:-.}" "entry '$pname' source resolves: $psource"
      ;;
    *) fail "entry '$pname' source must start with ./ or be a remote object: $psource" ;;
  esac
  # A marketplace-root source means this repo is the plugin, so its manifest
  # must be the one we validated above.
  if [ "$psource" = "./" ]; then
    found_self=1
    assert_file "$MANIFEST" "entry '$pname' resolves to the manifest under test"
    assert_eq "$pname" "$(json_str "$MANIFEST" name)" \
      "entry '$pname' name matches plugin.json (install id: $pname@$(json_str "$MARKET" name))"
  fi
done < <(printf '%s\n' "$entries")
ok "[ $found_self -eq 1 ]" "marketplace serves this repo as a plugin (source './')"

assert_summary
exit
