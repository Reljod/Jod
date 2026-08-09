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

_Notes on what actually runs there to be filled in as the box's role solidifies._
