---
description: Register Jod's MCP server with the harnesses on this machine, so hand-started sessions get Jod's tools.
argument-hint: "[--dry-run] [--harness claude|opencode|agy] [--access read_only|delegate|orchestrate]"
---

Give the sessions Reljod starts by hand the same Jod tools a Jod-spawned run
already gets — `schedule_create`, `schedule_list`, `delegate`, `remember`,
`recall` and the rest.

The gap this closes: `jod` hands every run *it* spawns an MCP config on the
command line, so those agents hold Jod's tools. Nothing hands one to the
`claude` you type in a repo, so that session has no way to schedule anything —
which reads like the scheduler not existing rather than like a missing config
entry.

Run it:

```sh
jod mcp install --dry-run $ARGUMENTS   # what it would touch
jod mcp install $ARGUMENTS             # do it
```

Then:

1. Show the user the printed table — one line per harness, with the config file
   Jod edited. Do not paraphrase the paths; these are the user's own files.
2. If a harness reports an error, report it **verbatim** and stop for that one.
   A config Jod cannot parse is left untouched on purpose (an `opencode.jsonc`
   with comments is the usual cause) — the fix is to add the `jod` entry by
   hand, not to have Jod rewrite the file. `--dry-run` prints the entry to
   paste.
3. Tell them to restart any already-open session: a harness reads its MCP
   config at startup, so a running one will not see the new server.
4. Verify from a *new* session rather than claiming success — in Claude Code,
   `/mcp` lists the server, and the tools appear as `mcp__jod__*`.

Notes worth passing on if they ask:

- **Access defaults to `orchestrate`**, the full set, because a session the user
  opened is one they are watching. Unattended and webhook-triggered runs are
  pinned to read-only where they are spawned and this cannot widen them —
  scheduling from a scheduled run is how a schedule multiplies overnight.
- **It is already automatic.** `jod daemon` runs the same registration on every
  start, which is what keeps the entry pointing at the current binary across an
  upgrade. This command is for doing it now, on a box with no daemon, or for
  a non-default `--access`.
- **Uninstalled harnesses are skipped**, so nothing creates a config directory
  for a program that is not there. `--all` overrides that; `--harness <one>`
  targets one and skips the check.
