# Which Linux to run Jod on — AlmaLinux vs CentOS vs Debian vs Ubuntu

**Date:** 2026-08-09 · **Analyst:** Jod · **Companion to:**
[`research/vps-comparison-2026`](../vps-comparison-2026/REPORT.md) (which host to buy)

> **Goal, as stated:** the best Linux to host the Jod agent on a VPS — fast,
> maintainable, unrestricted, easy to use, fewest bugs, hardest to abuse if the
> agent is turned against me, best features and support.

---

## The answer

**Ubuntu 26.04 LTS (Resolute Raccoon).** Runner-up: **Debian 13 "trixie"** if you
want the smallest number of moving parts. **AlmaLinux 10** only if something
external demands RHEL compatibility. **CentOS Stream 10: disqualified** — see below.

The deciding factor is not any of the seven criteria on their own. It is that
Jod drives a headless browser, and **Playwright officially supports Debian and
Ubuntu only** — `playwright install-deps` has no `dnf` package mapping at all.
On AlmaLinux you hand-resolve ~60 shared libraries and re-resolve them every
Chromium bump. That is a permanent maintenance tax paid in the exact place the
[IP-blocking research](../vps-comparison-2026/REPORT.md) says Jod is most
fragile.

Between the two Debian-family options, Ubuntu 26.04 wins on kernel recency
(7.0 vs 6.12), on newer runtimes in the base repo, and on a ten-year security
window that Debian 13 does not offer. Debian wins on "nothing changes under
you". Both are correct; Ubuntu is correct *more often* for this workload.

---

## Disqualified before scoring

**CentOS Stream 10 — do not use for this.** Stream is upstream *of* RHEL, not a
rebuild of it: it is a rolling preview where the next minor release lands
first. Fine for validating software against future RHEL. Wrong for the machine
that *is* your assistant, because:

- No bug-for-bug stable target — packages move ahead of RHEL continuously.
- Shortest life of the four: **EOL 31 May 2030**, tied to when RHEL 10 leaves
  Full Support.
- The community that would have caught your bug moved to AlmaLinux/Rocky after
  the 2021 CentOS 8 termination. You would be debugging alone.

If you want "CentOS", the honest 2026 answer is AlmaLinux. Scored below in its
place.

---

## The scorecard

Weights reflect the goal as stated: this is a single machine running an agent
that must not become someone else's shell.

| Criterion | Weight | Ubuntu 26.04 | Debian 13 | AlmaLinux 10 | CentOS Stream 10 |
|---|---|---|---|---|---|
| Speed (kernel + runtime recency) | 15% | **5** | 3 | 3 | 4 |
| Maintainability (upgrades, lifecycle) | 20% | **5** | 4 | **5** | 1 |
| Freedom / "can do anything" | 10% | 4 | **5** | 4 | 4 |
| Ease of use (docs, agent tooling) | 20% | **5** | 4 | 2 | 2 |
| Fewest bugs / issues | 15% | 4 | **5** | 4 | 2 |
| Resistance to agent abuse | 10% | 4 | 3 | **5** | 4 |
| Features & support | 10% | **5** | 3 | 4 | 2 |
| **Weighted total** | | **4.65** | 3.90 | 3.75 | 2.50 |

*Ratings 1–5, higher is better. Filters applied before scoring: must be a
stable (non-rolling) train — removes CentOS Stream; must have first-class
Playwright/Chromium dependency support — heavily penalises AlmaLinux.*

---

## Criterion by criterion

### Speed — Ubuntu

Not benchmark speed; **kernel and runtime speed**, which is what an agent
actually feels.

| | Kernel | Node (base repo) | Python |
|---|---|---|---|
| Ubuntu 26.04 | **7.0** | 22.x | **3.14** |
| Debian 13 | 6.12 LTS | 20.19 | 3.13 |
| AlmaLinux 10 | 6.12 | 22 | 3.12 |
| CentOS Stream 10 | 6.12 | 22 | 3.12 |

Ubuntu's kernel 7.0 brings newer io_uring, better cgroup v2 accounting, and a
later Landlock ABI — all of which matter when you are running dozens of
short-lived subprocesses and a browser. Debian 13's 6.12 is an LTS kernel whose
*upstream* support ends December 2026; Debian carries it themselves after that,
which is fine but means the kernel gets no new capabilities for five years.

Ubuntu also ships CUDA and ROCm through `apt`, which is free option value if
Jod ever runs a local model.

### Maintainability — Ubuntu and AlmaLinux tie

| | Full support | Security-only | Total |
|---|---|---|---|
| Ubuntu 26.04 LTS | Apr 2031 | Apr 2036 (Ubuntu Pro, **free** ≤5 machines) | **10 yr** |
| Debian 13 | Aug 2028 | Jun 2030 (LTS) | 5 yr |
| AlmaLinux 10 | May 2030 | May 2035 | **10 yr** |
| CentOS Stream 10 | — | May 2030 | 5 yr |

Ubuntu Pro is free for personal use on up to five machines and covers the
universe repo — that is a genuine ten-year window at zero cost, and it is the
single biggest thing Debian cannot match. Debian 13 puts a distro upgrade on
your calendar for 2028; Ubuntu and Alma put it in 2030+.

Both Debian-family options do in-place major upgrades reliably. AlmaLinux does
in-place major upgrades through ELevate, which works but is a project, not an
afternoon.

### Freedom — Debian, narrowly, and it does not matter

All four are free to run anything. There is no distro here that restricts what
you may execute, host, or scrape.

- **Debian** is the most ideologically clean: main is 100% DFSG-free, non-free
  is opt-in and explicit.
- **Ubuntu** ships restricted/multiverse and pushes snaps for some packages
  (notably `firefox`, `certbot`); on a headless server this is avoidable but
  you should know it exists.
- **AlmaLinux** is fully free and permissive; the extra software you want lives
  in EPEL, which you enable by hand.

**The real constraint is the host, not the OS** — the [VPS
study](../vps-comparison-2026/REPORT.md) already settled that. No distro choice
recovers a provider that suspends you for automated browsing.

### Ease of use — Ubuntu, by a wide margin

This is where AlmaLinux loses the comparison outright for *this* workload:

- **Playwright supports Debian 12/13 and Ubuntu 22.04/24.04/26.04 only.**
  `playwright install-deps` has no `dnf`/`microdnf` mapping — [open feature
  request since 2023](https://github.com/microsoft/playwright/issues/23949),
  still open. On Alma you resolve Chromium's shared-library set by hand, or you
  run the browser inside a Debian container, which is admitting the point.
- Every Claude Code / Node / uv / Docker install guide on the internet is
  written `apt`-first.
- Ubuntu's docs and Q&A corpus is the largest of the four by a long way.

Debian is nearly identical here; it just ships older runtimes, so you add
NodeSource or `mise` on day one instead of day never.

### Fewest bugs — Debian

Debian's freeze process is the strictest of the four and trixie has had a year
of production exposure since August 2025. Ubuntu 26.04 is four months old at
time of writing; 26.04.1 (August 2026) is the point release where the early
edges get sanded off. Two Ubuntu 26.04 specifics worth knowing before you
provision:

- **`sudo-rs` is now the default `sudo`.** Memory-safe, but not 100%
  feature-parity with sudo-C — exotic `sudoers` directives can behave
  differently. For a single-admin box this is a non-issue.
- **`rust-coreutils` replaces GNU coreutils.** Overwhelmingly compatible;
  occasional edge-case flag differences bite shell scripts.

Neither is disqualifying, and both are reasons to run 26.04.1+ rather than
26.04.0. If either makes you uneasy, that is the honest argument for Debian 13.

### Resistance to agent abuse — AlmaLinux on defaults, Ubuntu in practice

**Threat model:** the agent is the risk. A prompt injection in a scraped page
becomes an arbitrary shell command run by *your* trusted process. Nothing at
the OS layer prevents that. What the OS decides is how far the blast goes.

| | Default MAC | Coverage | User namespaces |
|---|---|---|---|
| AlmaLinux 10 | **SELinux, enforcing, targeted** | 217 LSM hooks | Unrestricted |
| Ubuntu 26.04 | AppArmor, enforcing | 80 LSM hooks | **Restricted by default** |
| Debian 13 | AppArmor, enforcing | 80 LSM hooks | Unrestricted |
| CentOS Stream 10 | SELinux, enforcing | 217 LSM hooks | Unrestricted |

**AlmaLinux genuinely wins on out-of-the-box confinement.** SELinux enforcing
with a targeted policy plus rootless Podman with SELinux labelling is the
strongest default of the four, and it is not close.

Ubuntu closes the gap for the reasons that decide real outcomes:

- **Kernel 7.0 → later Landlock ABI**, including network and `ioctl`
  restrictions. Landlock is the *right* primitive for confining an agent's
  subprocesses, because it is unprivileged — the agent's own launcher can drop
  its own filesystem and network rights before `exec`. Debian's 6.12 and Alma's
  6.12 give you an older ABI.
- **Restricted unprivileged user namespaces** by default
  (`apparmor_restrict_unprivileged_userns`), which removes a large class of
  local privilege-escalation chains. **Gotcha:** this also breaks Chromium's
  own sandbox and rootless `bwrap` unless you grant an AppArmor profile —
  handled in the setup below.
- **containerd 2.2, runc 1.4, docker.io 29** in the base repo, plus
  strengthened cgroup mount options. On Alma you add Docker's own repo.
- OpenSSH 10.2 with **hybrid post-quantum key exchange**
  (`mlkem768x25519-sha256`) on by default, DSA gone, legacy TLS off.

The containment that actually protects you is the same on all four and is where
your effort belongs: **run the agent as a non-root user, inside a hardened
systemd unit or a container, with an egress allowlist.** A `systemd-analyze
security` score below 3.0 buys more safety than the SELinux-vs-AppArmor choice
does.

### Features and support — Ubuntu

Largest package set, CUDA/ROCm via apt, free ten-year Pro/ESM, live kernel
patching available, the broadest third-party support matrix (Docker, Tailscale,
NodeSource, Playwright, Cloudflare all list Ubuntu first). AlmaLinux's
compensating strength is real but irrelevant here: it is the distro that
control panels and enterprise ISVs certify against, and Jod uses neither.

One AlmaLinux advantage worth recording in case you land on cheap hardware:
**RHEL 10 requires x86-64-v3 (AVX2), AlmaLinux 10 also ships an x86-64-v2
build.** On a budget VPS running Nehalem/Bulldozer-era silicon, AlmaLinux boots
where RHEL 10 and its other rebuilds will not. Ubuntu and Debian both still
target baseline x86-64, so they are unaffected.

---

## Recommendation

**Provision Ubuntu 26.04 LTS**, take the 26.04.1 point release or later, and
spend the saved setup time on the systemd hardening and egress allowlist below —
that is where the security actually is.

**Switch to Debian 13** if you would rather have a base that will not change
under you for five years and you accept a distro upgrade in 2028.

**Switch to AlmaLinux 10** only if a specific dependency demands RHEL, or the
VPS you bought has pre-v3 hardware — and then run the browser in a Debian
container.

---

## Running them

Everything below assumes a fresh 2 vCPU / 4 GB / 40 GB NVMe KVM VPS from the
[host study](../vps-comparison-2026/REPORT.md), reached as root over SSH.

### Ubuntu 26.04 LTS — full provisioning

```bash
#!/usr/bin/env bash
set -euo pipefail

### 1. Patch, then create the account the agent runs as ─────────────────
apt-get update && apt-get -y full-upgrade
adduser --disabled-password --gecos "" jod
install -d -m 700 -o jod -g jod /home/jod/.ssh
install -m 600 -o jod -g jod /root/.ssh/authorized_keys /home/jod/.ssh/authorized_keys
# jod is deliberately NOT in the sudo group — you administer as a separate
# human account. The agent must never be one `sudo` away from root.
adduser --disabled-password --gecos "" admin && usermod -aG sudo admin
install -d -m 700 -o admin -g admin /home/admin/.ssh
install -m 600 -o admin -g admin /root/.ssh/authorized_keys /home/admin/.ssh/authorized_keys

### 2. SSH: keys only, no root ──────────────────────────────────────────
cat >/etc/ssh/sshd_config.d/10-hardening.conf <<'EOF'
PermitRootLogin no
PasswordAuthentication no
KbdInteractiveAuthentication no
AllowUsers admin
EOF
sshd -t && systemctl reload ssh   # sshd -t first: a typo here locks you out

### 3. Firewall: deny inbound except SSH ────────────────────────────────
apt-get -y install ufw
ufw default deny incoming
ufw default allow outgoing
ufw allow OpenSSH
ufw --force enable

### 4. Unattended security updates ──────────────────────────────────────
apt-get -y install unattended-upgrades
cat >/etc/apt/apt.conf.d/20auto-upgrades <<'EOF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
EOF
# Free 10-year ESM — register at ubuntu.com/pro (free for ≤5 machines)
# pro attach <TOKEN> && pro enable esm-apps livepatch

### 5. Runtimes ─────────────────────────────────────────────────────────
apt-get -y install git curl ca-certificates build-essential docker.io
curl -fsSL https://deb.nodesource.com/setup_24.x | bash -
apt-get -y install nodejs
sudo -u jod bash -lc 'curl -LsSf https://astral.sh/uv/install.sh | sh'
npm install -g @anthropic-ai/claude-code

### 6. Headless Chromium for the browsing agent ─────────────────────────
sudo -u jod bash -lc 'npx playwright install --with-deps chromium'
# Ubuntu 24.04+ restricts unprivileged user namespaces, which breaks
# Chromium's own sandbox. Grant it a profile rather than disabling the
# sandbox with --no-sandbox, which would be strictly worse:
cat >/etc/apparmor.d/chrome-jod <<'EOF'
abi <abi/4.0>,
include <tunables/global>
profile chrome-jod /home/jod/.cache/ms-playwright/**/chrome flags=(unconfined) {
  userns,
  include if exists <local/chrome-jod>
}
EOF
apparmor_parser -r /etc/apparmor.d/chrome-jod
```

Then the hardened service. This is the part that matters:

```ini
# /etc/systemd/system/jod.service
[Unit]
Description=Jod orchestrator
After=network-online.target
Wants=network-online.target

[Service]
User=jod
Group=jod
WorkingDirectory=/home/jod/jod
EnvironmentFile=/etc/jod/env          # chmod 600 root:root — secrets live here
ExecStart=/usr/bin/node /home/jod/jod/server.js
Restart=on-failure
RestartSec=5s

# ── containment ────────────────────────────────────────────────────────
NoNewPrivileges=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/home/jod/jod/state /home/jod/.cache
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectProc=invisible
RestrictSUIDSGID=yes
RestrictRealtime=yes
LockPersonality=yes
CapabilityBoundingSet=
SystemCallFilter=@system-service
SystemCallArchitectures=native

# ── egress allowlist: the agent talks to these and nothing else ────────
# Anthropic publishes fixed IPs and states they will not change without
# notice — re-check platform.claude.com/docs/en/api/ip-addresses before use.
IPAddressDeny=any
IPAddressAllow=localhost
IPAddressAllow=160.79.104.0/23        # api.anthropic.com (inbound range)
IPAddressAllow=2607:6bc0::/48         # same, IPv6
IPAddressAllow=140.82.112.0/20        # github.com
# add each destination deliberately; this is your strongest single control.
# Note: this cannot survive a browsing agent that must reach arbitrary sites —
# put that worker in a separate unit with its own (wider) policy, or front it
# with a filtering proxy. Do not widen this unit to `any` to make one job work.

[Install]
WantedBy=multi-user.target
```

```bash
systemctl daemon-reload && systemctl enable --now jod
```

**Two deliberate omissions, so you do not re-add them and break things:**

- **`RestrictNamespaces=~CLONE_NEWUSER` is absent.** Chromium's sandbox needs
  user namespaces. Restrict namespaces only on units that never launch a
  browser; put the browsing worker in its own unit or its own container.
- **`MemoryDenyWriteExecute=yes` is absent.** V8 JIT-compiles, so W^X breaks
  Node outright.

**Runnable check** — verify the hardening instead of trusting it:

```bash
systemd-analyze security jod.service   # 0–10, lower is better; this unit
                                       # should land in the low single digits.
                                       # Compare before/after to prove the
                                       # hardening block is actually applied.
node -v && uv --version && claude --version
sudo -u jod npx playwright screenshot https://example.com /tmp/t.png  # browser sandbox works
ufw status verbose
aa-status | head -3
journalctl -u jod -n 50 --no-pager
```

### The same box on the other three

Only the steps that differ. Sections 1–2 (users, SSH) are identical everywhere.

| Step | Ubuntu 26.04 | Debian 13 | AlmaLinux 10 |
|---|---|---|---|
| Update | `apt-get -y full-upgrade` | `apt-get -y full-upgrade` | `dnf -y upgrade` |
| Firewall | `ufw allow OpenSSH && ufw enable` | `apt install ufw` then same | `firewall-cmd --permanent --add-service=ssh && firewall-cmd --reload` |
| Auto-updates | `unattended-upgrades` | `unattended-upgrades` (enable `trixie-security`) | `dnf -y install dnf-automatic && systemctl enable --now dnf-automatic.timer` |
| Extra repos | none | none | `dnf -y install epel-release && dnf config-manager --set-enabled crb` |
| Node 24 | NodeSource `deb` | NodeSource `deb` | `dnf -y install nodejs` (22) or NodeSource rpm — **not** `dnf module`, modularity is gone in RHEL/Alma 10 |
| Docker | `apt install docker.io` (v29 in base) | `apt install docker.io` | Docker CE repo, or **`podman`** (rootless + SELinux — preferred here) |
| Chromium deps | `playwright install --with-deps` | `playwright install --with-deps` | ⚠️ **unsupported** — resolve by hand, or run the browser in a Debian container |
| MAC | AppArmor (`aa-status`) | AppArmor (`aa-status`) | SELinux (`sestatus`) — **leave enforcing** |
| Userns gotcha | AppArmor profile needed (above) | none | none |

**AlmaLinux browser workaround**, if you go that route — keep the RHEL host and
put Chromium where it is supported:

```bash
dnf -y install podman
podman run -d --name jod-browser \
  --userns=keep-id --security-opt label=type:container_runtime_t \
  -p 127.0.0.1:3000:3000 \
  mcr.microsoft.com/playwright:v<YOUR-PLAYWRIGHT-VERSION>-noble \
  npx -y playwright run-server --port 3000 --host 0.0.0.0
```

Pin the image tag to the exact Playwright version your client uses — a
client/server mismatch fails at connect time with a protocol error.

Then point the agent's Playwright client at `ws://127.0.0.1:3000/`. This is
correct and it works — it is just Ubuntu, one layer down, which is the argument
for provisioning Ubuntu in the first place.

---

## Sources

- [AlmaLinux 10 vs Ubuntu 26.04 vs Debian 13 — RoseHosting](https://www.rosehosting.com/blog/almalinux-10-vs-ubuntu-26-04-vs-debian-13-which-linux-distro-for-your-vps-in-2026/)
- [Choosing a Linux Server Distribution in 2026](https://knightli.com/en/2026/05/07/linux-server-distro-comparison-2026/)
- [What's new in security for Ubuntu 26.04 LTS — Canonical](https://canonical.com/blog/ubuntu-26-04-lts-security-updates)
- [Ubuntu 26.04 LTS release notes](https://documentation.ubuntu.com/release-notes/26.04/)
- [Debian 13 "trixie" release information](https://www.debian.org/releases/trixie/)
- [Debian 13.0 released, powered by Linux 6.12 LTS — Phoronix](https://www.phoronix.com/news/Debian-13.0-Released)
- [AlmaLinux 10.0 release notes](https://wiki.almalinux.org/release-notes/10.0.html)
- [AlmaLinux 10.0 continues supporting x86-64-v2 CPUs — Phoronix](https://www.phoronix.com/news/AlmaLinux-10.0-Released)
- [CentOS Stream 10 release notes](https://www.centos.org/centos10/)
- [CentOS vs Red Hat 2026 — production guidance](https://www.golinuxcloud.com/centos-vs-redhat/)
- [Playwright — supported browsers and platforms](https://playwright.dev/docs/browsers)
- [Playwright #23949 — non-Ubuntu Linux dependency install](https://github.com/microsoft/playwright/issues/23949)
- [Playwright #41318 — official Fedora/RHEL support request](https://github.com/microsoft/playwright/issues/41318)
- [Landlock — unprivileged sandboxing](https://landlock.io/)
- [Anthropic API IP addresses](https://platform.claude.com/docs/en/api/ip-addresses)
- [RHEL 10 deprecated features — modularity](https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/10/html/10.1_release_notes/deprecated-features)
- [SELinux vs AppArmor — LSM hook coverage](https://linuxsecurity.com/news/security-trends/selinux-vs-apparmor-uptake-trends-security-considerations)
