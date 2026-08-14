# SPEC — daemon mode: work continues, the console comes and goes

## Why this exists

Asked for: *"I should be able to run the sessions in background even if I close
the jod tui."*

**The process half of that already works, and was verified before this spec was
written.** A run is a `setsid`-detached `jod-run` supervisor writing straight
into `jod.db`; closing the console takes nothing with it. Measured on the
installed binaries, not inferred from the code:

```
$ jod run -n bgtest "run the shell command: sleep 45; echo BACKGROUND_SURVIVED"   # launcher pid 6902
$ kill -9 6902                                                                    # launcher dead
  PID  PGID STAT COMMAND
 6906  6906 SNs  jod-run …/spawn.json          ← survived
 6907  6906 SN   claude -p …                   ← survived
 6954  6906 SN   …/jod_browser_mcp.py          ← survived
$ sqlite3 ~/.jod/jod.db "select status from runs where id='b9d7f6d9…'"
completed
```

So this spec is **not** about making runs detach. It is about the three things
that make a person believe they did not:

1. **Nothing is resident.** Everything that repairs, reaps or advances state
   lives in `jod daemon`, which nobody is running. Close the console and no
   schedule fires, no wedged run is reaped, no delivery goes out. Work that was
   *already running* continues; work that was *waiting* does not.
2. **Coming back shows nothing.** `App::new` sets `watching: None` and the event
   loop never sets it. Reopen the console with a live agent and it sits in the
   fleet behind an empty transcript until you press `⏎` on it.
3. **`rehydrate(200)`.** A live run older than the newest 200 rows is never
   loaded, so it is absent from the fleet and `kill_agent` answers
   `UnknownAgent` for it.

## The constraint that shapes everything below

`docs/decisions.md` — *"A run is a detached process group, and the database is
its only transport"* — is settled, and this must not regress it. **The daemon
does not own runs.** It must be killable at any moment without touching a
single agent, exactly as the console is today. It is a resident *manager*
beside the runs, never above them.

Any design where runs become children of the daemon is out of scope and out of
order: it reintroduces the coupling the tmux removal existed to delete.

## Scope

### 1. `jod daemon` becomes something a person actually has running

Today it is a foreground process a person must remember to start. It needs to
be installed, supervised by the OS, and started on login.

- `jod daemon install` / `uninstall` / `status` — writes a launchd plist on
  macOS (`~/Library/LaunchAgents/ai.jod.daemon.plist`, `KeepAlive`,
  `RunAtLoad`), a systemd user unit on Linux (`~/.config/systemd/user/`).
  `deploy/` already has VPS material to reuse.
- **Single instance.** Two daemons double-fire every schedule. `ticker::LEASE_MS`
  guards a *tick*, not a process; add a pidfile or an advisory lock on the
  store, and make the second one exit saying where the first is.
- `jod daemon status` reports: running or not, pid, uptime, last tick, how many
  live runs it can see. This is the command that answers "is my stuff being
  looked after".

**Check:** kill the daemon; it comes back by itself within 10s and no run's
status changes. A test that asserts run rows are untouched across a daemon
restart is the one that protects the constraint above.

### 2. The console attaches and detaches, and says which

- **On startup, adopt what is live.** If exactly one run is live, watch it. If
  several, say so and offer the picker rather than opening an empty transcript.
  The information is already there — `rehydrate` sets
  `summary.process_alive`; nothing reads it at startup.
- **A detach key** (`Ctrl-D`, subject to the chord audit in #75) that leaves
  everything running and says so on the way out, distinct from quitting.
- **Quit already warns** (`on_quit`, *"press again to leave them running"*).
  Keep it, and make the wording match the new vocabulary: attached / detached,
  not running / stopped.

**Check:** launch a run, kill the console with `SIGKILL`, restart it, and the
transcript is live again with no keypress. Assert against the store, in a test
that never opens a TTY — the seam `cli/src/tui/data.rs` already keeps.

### 3. Remove the caps and the misjudgements that make live work look dead

- **`rehydrate(200)`** — load every run whose status is still `running`
  regardless of age, then the newest N finished ones for history. The cap is
  right for a transcript and wrong for the live set.
- **`kill_agent` has no store fallback** while `fail_agent` does
  (`core/src/service.rs`). A run the process did not launch and did not
  rehydrate cannot be stopped. Give it the same fallback.
- **`core/src/proc.rs` `group_alive` probes the leader, not the group** — it
  runs `kill(pgid, 0)` while its own doc promises *"any process in the group"*,
  and `signal_group` two functions above correctly uses `kill(-pgid, …)`. A
  supervisor that is SIGKILLed or OOM-killed while its harness runs on makes Jod
  declare the run dead, and **with the daemon resident that becomes a persisted
  `failed` row while the harness keeps spending money.** Section 1 turns this
  from latent into live, so it is a prerequisite, not a nice-to-have. The fix
  must preserve the existing zombie semantics — a group holding nothing but an
  unreaped corpse is still dead.
- **`supervisor/src/main.rs` `stop()`** calls `signal_group(child.id(), …)`, but
  the harness inherits the supervisor's pgid rather than leading one, so
  `kill(-child_pid, …)` names a group that does not exist, returns `ESRCH`, and
  `signal_group` swallows it as success. The 5s fallback then SIGKILLs the
  direct child only, orphaning its descendants — the `jod_browser_mcp.py` in the
  transcript above is exactly such a descendant.

**Check:** SIGKILL a supervisor while its fake harness runs on; `group_alive`
must still answer true, and the daemon must not mark the run failed.

## Explicitly out of scope

- Runs becoming children of the daemon — see the constraint.
- Replacing `jod-api` or moving the TUI onto HTTP. The daemon and the console
  keep talking through `jod.db`, which is what lets either die alone.
- Multi-machine. One box, one store.

## Open questions — answer before executing

1. **Does the daemon start runs the console asked for, or does the console keep
   starting its own?** Today the console spawns directly and that works with the
   daemon absent. Routing spawns through the daemon would make it a hard
   dependency of typing a message. *Assumed answer: the console keeps spawning
   its own; the daemon never becomes required for a turn.* Confirm.
2. **Should `jod tui` start the daemon if it is missing?** Silent auto-start is
   convenient and makes the failure mode invisible. *Assumed: no — offer it and
   record the choice, the way the missing-supervisor path already refuses
   loudly.* Confirm.
3. **Login-start by default on install, or opt in?** *Assumed: opt in, because
   `install.sh` currently installs binaries and nothing resident.*
4. **What does the daemon do about a run it finds wedged?** The heartbeat sweep
   exists; nothing has decided whether the default is reap or report.

## Order of work

Section 3 first — it is small, it is testable without any new process, and two
of its items get *worse* the moment section 1 lands. Then 1, then 2.

## Prior art in-tree

`core/src/daemon.rs` (the resident loop and its rationale), `core/src/ticker.rs`
(leases), `core/src/heartbeat.rs` (the sweep), `deploy/` (VPS units),
`supervisor/tests/survives_its_parent.rs` (the fixture that proves a thing
outlives its launcher — every check above should use it).
