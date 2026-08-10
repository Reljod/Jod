# Infra

System of record: **the box itself** (plus `~/.ssh/config` for how to reach it).

## One host, four names

Reljod uses **VPS**, **Jod Cloud**, **Jarvis**, and **cloud** interchangeably.
All four mean the same single remote machine — its own hostname is `Jod`. When
he says "update the cloud" or "check Jarvis", there is nothing to disambiguate:

```sh
ssh jod-cloud
```

## The address is not in this repo

`jod-cloud` is an alias defined in Reljod's `~/.ssh/config`, which is where the
real user and address live. This repo is public, so it never spells out the
host. → [why](../../docs/decisions.md#the-vps-address-lives-in-ssh-config-not-in-the-repo)

If `ssh jod-cloud` fails with `Could not resolve hostname`, the alias is missing
on this machine rather than the box being down — ask Reljod for the entry, don't
go hunting for an IP to hard-code.

## What the agent may do there

Standard [charter rules](../../AGENTS.md#principles) apply, and this domain is
the one where "reversible by default" bites hardest — a laptop mistake is local,
a mistake here is a service other things depend on.

- **No check-in needed:** reading state — `systemctl status`, `journalctl`,
  `docker ps`, `df`, `tmux ls`, reading config and logs.
- **Confirm first:** anything that changes running state — restarting or
  stopping services, editing config that a service reads, package installs and
  upgrades, firewall or SSH changes, deleting anything, rebooting.

## When an agent "goes idle"

Check these in order. The cheap one is nearly always the answer, and the
tempting one nearly always is not.
→ [why](../../docs/decisions.md#an-idle-agent-is-usually-a-full-disk-not-a-dropped-ssh)

```sh
df -h /                      # 1. full disk — the usual culprit
tmux ls                      # 2. session gone -> the agent died, not stalled
ss -tanpo | grep claude      # 3. ESTAB with a stuck timer -> half-open socket
```

**A full root filesystem presents as an idle agent, not as an error.** The agent
cannot write its transcript, shell snapshot or SQLite commit, so it stalls
silently. `df` first, every time.

The disk fills from one place: `.claude/worktrees/<job>/target`. Every background
job takes a worktree and cargo-builds 2–5GB into it, and nothing removes it
afterwards. `target/` is gitignored and regenerable, so it is safe to delete —
but never delete one belonging to a *locked* worktree, which means a live
session still holds it:

```sh
du -xsh /home/reljod/repo/Jod/.claude/worktrees/*/target | sort -rh
git worktree list                    # anything marked `locked` is in use — leave it
rm -rf /home/reljod/repo/Jod/.claude/worktrees/<name>/target
```

## Session persistence — what is configured and why

Applied on the box; all four survive reboot.

| Setting | Value | File |
|---|---|---|
| SSH keepalive | `ClientAliveInterval 30`, `CountMax 6` | `/etc/ssh/sshd_config.d/70-keepalive.conf` |
| TCP keepalive | `time 300`, `intvl 15`, `probes 4` | `/etc/sysctl.d/99-tcp-keepalive.conf` |
| Linger | `enabled` for `reljod` | `loginctl enable-linger` |
| mosh | installed, UDP `60000:61000` open | `ufw` |

Two things worth knowing before changing any of it:

- **sshd here is socket-activated** (`ssh.socket` enabled, `ssh.service`
  disabled), so it re-reads its config on every new connection. No reload is
  needed and established sessions are never disturbed — but a syntax error
  breaks *new* logins while you are still comfortably connected. Always
  `sudo sshd -t` before trusting a change, and verify with
  `sudo sshd -T | grep -i clientalive`.
- **`tcp_keepalive_time` does not affect the agents.** Node sets `TCP_KEEPIDLE`
  per socket and overrides it; only `intvl` and `probes` reach them.

Run agents inside tmux (`tmux new -As <name>` — attaches or creates), or
headless via `claude -p` under a systemd user unit, which needs the linger above
and drops the TTY dependency entirely.
