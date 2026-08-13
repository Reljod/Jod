# Deploying `jod-api` on the VPS

Putting this daemon on a machine puts an endpoint that **spawns agent harnesses**
on that machine. An agent harness runs shell commands, so a credential for this
API is arbitrary code execution as the `jod` user. Everything below follows from
taking that seriously. → [`docs/jod-api.md`](../docs/jod-api.md)

The box is reached as `ssh jod-cloud` — an alias in `~/.ssh/config`. This repo is
public and never spells out the address.
→ [why](../docs/decisions.md#the-vps-address-lives-in-ssh-config-not-in-the-repo)

`install.sh` does **not** install `jod-api` unless asked (`JOD_WITH_API=1`).
Standing up an RCE endpoint should be a deliberate act, never a side effect of
installing a CLI.

## The shape

```
    phone / browser
          │  HTTPS, WireGuard, device-authorised
    ┌─────▼──────────────┐
    │  tailnet           │   no inbound port open to the internet
    └─────┬──────────────┘
          │  tailscale serve (TLS termination, real *.ts.net cert)
    ┌─────▼──────────────┐
    │ 127.0.0.1:8787     │   jod-api — loopback only
    └─────┬──────────────┘
          │
      jod-core ──► jod-run ──► claude / opencode / agy
```

Nothing listens on a public interface at any point.

## 1. A dedicated user

The daemon must not run as root, and must not run as a human account that has
SSH keys and dotfiles worth stealing.

```sh
sudo useradd --system --create-home --shell /usr/sbin/nologin jod
sudo -u jod mkdir -p /home/jod/.jod /home/jod/work
```

`/home/jod/work` is the only place agents will be allowed to run. Anything the
agent should reach — a repo checkout — goes under it.

## 2. Install the binaries

`install.sh` does the whole job — clone, build, install — as the `jod` user, so
the checkout it later updates from is one that user owns:

```sh
sudo -u jod env JOD_HOME=/home/jod/.jod JOD_BIN_DIR=/usr/local/bin JOD_WITH_API=1 \
  bash -c 'curl -fsSL https://raw.githubusercontent.com/Reljod/Jod/main/install.sh | bash'
```

`JOD_WITH_API=1` is what adds `jod-api` to the default `jod` + `jod-run`;
without it the daemon is not installed at all. `/usr/local/bin` is root-owned,
so the installer escalates with `sudo` for the copy itself and says so.

Later, on the box: `jod update` takes newer patches, `jod update --check` says
what it would take first. It renames the new binaries over the old ones, so it
works while the console is running — but a running process keeps the build it
started with, so restart the units afterwards (the installer prints the exact
commands for whatever it finds running). From inside the console, `/update`
does the same thing as a background job and then offers to restart itself into
the new build; `Ctrl-G j` shows what is running.

**Install both `jod` and `jod-run`.** `jod-run` supervises every agent — it holds the run's output
and writes it to the store — so `jod-api` without it can serve requests but
cannot start anything. It is looked for beside the running executable first,
then on `PATH`; `JOD_SUPERVISOR_BIN` overrides both. Keep the two at the same
version: they share the event and plan formats.

At least one harness (`claude`, `opencode`, or `agy`) must be on the `jod`
user's `PATH`.

Every installed harness is registered with Jod's own MCP server on daemon
start, so an upgrade that moves the binary re-points the configs by itself and
a session started by hand holds `schedule_create`, `delegate` and `remember`
like the main chat does. To do it without waiting for a restart, or to see what
it would touch first:

```sh
sudo -u jod jod mcp install --dry-run
sudo -u jod jod mcp install
```

It edits only its own entry, never a config it cannot parse, and re-running is
free. Set `JOD_NO_MCP_INSTALL=1` in the unit file on a box whose harness
configs are managed by something else.

```sh
sudo -u jod jod-api serve --bind 127.0.0.1:8787   # will warn about anything missing
```

## 3. Configure

`/home/jod/.jod/api.toml`:

```toml
bind = "127.0.0.1:8787"
max_permission = "accept_edits"
max_concurrent_agents = 8
allowed_cwd = ["/home/jod/work"]
max_body_bytes = 262144
session_ttl_hours = 168
```

Two lines carry most of the safety:

- **`allowed_cwd`** — agents may only run under these roots. Empty means *deny
  every spawn*, which is the correct default for an unset security control.
  Paths are canonicalised before the check, so `..` cannot escape.
- **`max_permission`** — the most permissive policy a remote caller may ask for.
  Leave it at `accept_edits`. Setting `bypass` auto-approves every tool call and
  turns the API into a remote shell; it should be a deliberate, temporary,
  locally-made change, never the resting state.

## 4. Mint tokens

One per device, so a lost phone is one revocation and not a rotation.

```sh
sudo -u jod jod-api token issue phone  --scope read     # watch only
sudo -u jod jod-api token issue laptop --scope write    # can spawn agents
sudo -u jod jod-api token list
sudo -u jod jod-api token revoke phone
```

The token is printed **once**. Put it in the device keychain, not a note or a
shell history. Tokens are stored as SHA-256 digests in
`/home/jod/.jod/api-tokens.json` (mode `0600`), so that file is not a credential.

**Give a `read` token to anything that only watches.** It is the single
highest-leverage control here: the credential most likely to leave the house
then cannot execute code.

## 5. The systemd unit

`/etc/systemd/system/jod-api.service` — see [`jod-api.service`](jod-api.service).

```sh
sudo cp deploy/jod-api.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now jod-api
systemctl status jod-api
journalctl -u jod-api -f
```

## 6. Publish to the tailnet

```sh
sudo tailscale up
sudo tailscale serve --bg 8787
tailscale serve status
```

`tailscale serve` terminates TLS with a real `*.ts.net` certificate and forwards
to loopback — so there is no certificate to manage and no port to open. Install
Tailscale on the phone, sign in to the same tailnet, and the API is at
`https://<host>.ts.net`.

**Do not use `tailscale funnel`.** Funnel publishes to the public internet, which
is exactly what this design exists to avoid — and identity headers are not
injected on funnel traffic.

Verify the negative, not just the positive:

```sh
curl -s https://<host>.ts.net/v1/health           # {"status":"ok"}
curl -s --connect-timeout 5 http://<public-ip>:8787/v1/health   # must fail
sudo ss -tlnp | grep 8787                         # must show 127.0.0.1 only
```

That third command is the one that matters. If it shows `0.0.0.0:8787`, stop and
fix it before going further.

## 7. Firewall

Belt and braces, in case the bind is ever misconfigured:

```sh
sudo ufw default deny incoming
sudo ufw allow in on tailscale0
sudo ufw allow 22/tcp        # keep your SSH, or you will lock yourself out
sudo ufw enable
```

## Checking it works

```sh
TOKEN=<the write token>
BASE=https://<host>.ts.net

curl -s $BASE/v1/health
curl -s -H "Authorization: Bearer $TOKEN" $BASE/v1/harnesses
curl -s -H "Authorization: Bearer $TOKEN" $BASE/v1/report

curl -s -X POST -H "Authorization: Bearer $TOKEN" \
     -H 'Content-Type: application/json' \
     -H "Idempotency-Key: $(uuidgen)" \
     -d '{"prompt":"say pong","permission":"ask"}' \
     $BASE/v1/agents

curl -N -H "Authorization: Bearer $TOKEN" $BASE/v1/agents/<id>/stream
```

And confirm the refusals actually refuse — a control you have not seen say *no*
is a control you are only assuming:

```sh
curl -s $BASE/v1/agents                                    # 401
curl -s -H "Authorization: Bearer $READ_TOKEN" \
     -X POST -d '{"prompt":"x"}' $BASE/v1/agents           # 403
curl -s -H "Authorization: Bearer $TOKEN" -X POST \
     -H 'Content-Type: application/json' \
     -d '{"prompt":"x","cwd":"/etc"}' $BASE/v1/agents      # 403
```

## Operating it

- **Audit:** `/home/jod/.jod/audit.jsonl` — one JSON line per mutating request,
  with the token *label*, never the token.
  `jq -r 'select(.outcome != "ok")' /home/jod/.jod/audit.jsonl` shows refusals;
  a run of `refused_scope` means a read credential is being probed.
- **Transcripts:** `/home/jod/.jod/jod.db`, readable whether or not the daemon
  is running — `sudo -u jod jod watch <id>` replays one. What was *asked* stays
  on disk as `/home/jod/.jod/runs/<id>/prompt.txt`, alongside the `spawn.json`
  recording exactly what was launched.
- **A supervisor that failed early:** `/home/jod/.jod/runs/<id>/supervisor.log`.
  A run that never produced an event left its reason there.
- **Restart:** `sudo systemctl restart jod-api`. Running agents survive — each
  is its own `setsid` process group, not a child of the daemon — and the daemon
  reloads prior runs from the store on boot, resuming its followers on the ones
  still alive. Browser sessions are dropped, which is deliberate.

## If a token leaks

```sh
sudo -u jod jod-api token revoke <label>
sudo systemctl restart jod-api      # also drops every browser session
grep '<label>' /home/jod/.jod/audit.jsonl    # what it did
sudo -u jod jod ls                           # what is still running
sudo -u jod jod kill <id>                    # stops the group, and its children
```

Then read the transcripts of anything that ran. Revocation stops future use; it
does not undo what already executed.

---

# The scheduler — `jod-daemon`

`jod-api` answers requests. Nothing in it looks at the clock, so a machine with
only `jod-api` on it has schedules that are stored, listed, and never fired.
`jod-daemon` is the process that ticks.

One tick, every 60 seconds — cron's own resolution, so polling faster buys
nothing and polling slower makes `* * * * *` a lie. Each tick claims what is
due, decides what to do about it, and lets go. → [`core/src/daemon.rs`](../core/src/daemon.rs),
[`core/src/ticker.rs`](../core/src/ticker.rs)

## Why a resident process and not a systemd timer

A timer invoking `jod tick` every minute is the smaller-looking design. It is
rejected because a one-shot process pays for opening the store, reloading prior
runs and re-parsing every armed cron expression *before* it can answer "is
anything due" — 1,440 times a day, to do 1,440 tiny reads. That startup cost
also puts a floor under how often you can tick at all. The full argument,
including what WAL mode has to do with it, is the module doc of
[`core/src/daemon.rs`](../core/src/daemon.rs).

The timer shape still works if you prefer it: a single tick is one call and one
exit code, and a claim left behind by a process that died mid-fire is recovered
after the 5-minute lease.

## Install

`jod-daemon` is a second unit over the **same** `jod` user, `/home/jod/.jod`
and binary set as `jod-api` — sections 1 and 2 above are its prerequisites, not
a separate setup. Running both at once is safe: they claim schedules out of one
SQLite file with a compare-and-swap and a lease, so a schedule cannot fire
twice.

```sh
sudo cp deploy/jod-daemon.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now jod-daemon
```

You do **not** need `jod-api` for the scheduler to work. You do need `jod-run`
installed, exactly as in section 2 — a schedule that fires spawns an agent, and
an agent without its supervisor produces nothing.

## Checking it is alive

`active (running)` is not evidence. A scheduler is the kind of process that
looks healthy while doing nothing at all, so check that it *ticked*:

```sh
systemctl status jod-daemon
journalctl -u jod-daemon -f            # a failing tick logs and carries on
sudo -u jod jod schedule ls            # next fire should move
sudo -u jod jod schedule log <name>    # one row per fire, skips included
```

The last one is the real check. Every decision writes a row — including the
ones that decided *not* to run — so "it never fired" and "it fired and was
skipped" are distinguishable. A schedule whose `next_fire_at` is in the past
and has no matching row is a daemon that is not ticking.

## Restarts and stopping

`SIGTERM` — what `systemctl stop` and `systemctl restart` send — is not acted on
until the tick in flight has finished. A claim abandoned between claiming a
schedule and firing it is precisely the case the lease exists to recover, and it
is better not to create it. `TimeoutStopSec=45s` bounds the wait.

Running agents survive a restart: each is its own process group, not a child of
this unit, and the daemon reloads prior runs from the store at boot. That reload
is load-bearing rather than cosmetic — the overlap policy asks "is a run from
this schedule still going", and a daemon that had forgotten would answer *no*
and start a second one.

A failing tick is logged and the loop continues. Ending on the first error would
leave the unit `active` with nothing firing, which is worse than having no
scheduler, because it looks like one.

## What has not been verified

**The unit file in this repo has never been installed or started on a machine.**
It is written against `jod-api.service`'s conventions and reviewed, not
observed — nobody has watched `systemctl enable --now jod-daemon` come up, seen
a schedule fire from it, or timed a `systemctl stop` against a tick in flight.
The same caveat the scheduling research carries about its own units applies
here. Treat the first install as a test, and check the fires table before
trusting it.

`ExecStart=/usr/local/bin/jod daemon` names a subcommand the CLI does have
(`jod daemon`, and `jod daemon --once` for a single tick) — but "the command
parses" is not "the unit runs", which is what the paragraph above is about.
