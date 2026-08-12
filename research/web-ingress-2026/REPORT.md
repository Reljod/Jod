# Exposing a web app on the VPS — how to do it without losing the box

**Date:** 2026-08-12 · **Analyst:** Jod · **Status:** research only —
**nothing on the box was changed.** Every fact under
[What is actually on the box](#what-is-actually-on-the-box-today) was measured
on `Jod` at the time of writing, not assumed.

---

## The short version

There is no single "secure way to expose a web app", because two different
questions are hiding inside that phrase. The answer forks on **who is allowed to
load the page**:

| If the audience is… | Do this | Public port opened |
|---|---|---|
| **Just you, and a few trusted devices** | Tailscale + `tailscale serve` | **none** |
| **Real users on the open internet** | Cloudflare Tunnel → loopback origin, Access in front of anything non-public | **none** |

Both recommended paths open **zero inbound ports**. That is not paranoia — it is
the cheapest way to make the whole category of "someone portscans 45.45.218.177
and finds your app server" stop existing. Anything that requires
`ufw allow 443` is a worse deal than either option above, and you can always add
it later.

Three things matter more than the choice between them:

1. **Fix SSH first.** The box currently accepts **password logins for `root`
   from the internet**, and is being probed ~600×/day. Adding a web surface to a
   box in that state is putting a new lock on a door that is already open.
   → [detail](#fix-this-before-you-expose-anything)
2. **Decide which app you mean.** `apps/web` is a control panel for an API whose
   own docs call it "arbitrary code execution on the box". That app has a
   different security bar than a blog. → [detail](#not-all-web-apps-are-the-same-app)
3. **The origin must never bind a public interface,** whichever tunnel you pick.
   The tunnel is not a firewall; if the app also listens on `0.0.0.0:3000`,
   attackers use that and skip the tunnel entirely.

---

## Fix this before you expose anything

This is the one finding in this report that is not hypothetical, and it is
independent of any web app. **I recommend fixing it whether or not you ever
expose anything.**

`sshd` on this box currently resolves to:

```
permitrootlogin yes
passwordauthentication yes
```

Both `root` and `reljod` have usable (unlocked) passwords — `passwd -S` reports
`P` for each. So the live configuration is: **anyone on the internet may attempt
to guess the root password over SSH, forever.**

### Why it looks fine but isn't

The Ubuntu cloud image ships hardened. The provider's cloud-init undid it:

```
/etc/ssh/sshd_config.d/50-cloud-init.conf   →  PasswordAuthentication yes
/etc/ssh/sshd_config.d/60-cloudimg-settings.conf → PasswordAuthentication no
```

OpenSSH takes the **first** value it obtains, not the last, and `50-` sorts
before `60-`. So the hardened default in `60-` is dead code — it is never
reached. Reading either file alone tells you the opposite of the truth, which is
why `sudo sshd -T` is the only trustworthy check here.

`PermitRootLogin yes` comes separately, from line 54 of the main
`/etc/ssh/sshd_config`; no drop-in overrides it.

### The exposure is being actively exercised

| Measure | Value |
|---|---|
| Failed SSH auth attempts, last 24h | **617** |
| fail2ban `sshd` total failures | 747 |
| IPs banned to date | 40 |

fail2ban is doing its job, but it is a **rate limiter, not an authenticator**.
Default settings still permit sustained slow guessing from a rotating IP pool,
and they do nothing against a password that appears in a credential dump.

### The fix

Three lines, and it costs nothing:

```sh
# /etc/ssh/sshd_config.d/10-hardening.conf   (10- sorts first, so it wins)
PermitRootLogin prohibit-password
PasswordAuthentication no
KbdInteractiveAuthentication no
```

```sh
sudo sshd -t                                  # syntax check — never skip
sudo sshd -T | grep -iE 'permitrootlogin|passwordauth'   # confirm it took
```

**Verify your key login works in a second terminal before closing the first.**
sshd here is socket-activated, so the change applies to new connections
immediately and existing sessions are untouched — that is your safety net, use
it. → [`domains/infra/README.md`](../../domains/infra/README.md)

This does not lock you out of console access; the provider's web console is
unaffected.

---

## What is actually on the box today

Measured, not assumed. This matters because
[`deploy/README.md`](../../deploy/README.md) documents a Tailscale-based
architecture that **is not installed** — it describes an intended end state, and
reading it as a description of reality would be a mistake.

| | State |
|---|---|
| OS | Ubuntu 26.04 LTS |
| Public IP | `45.45.218.177` on `eth0` |
| Listening on a public interface | **`:22` only** (sshd) |
| Listening on loopback | `agy` helper ports, systemd-resolved |
| `ufw` | **active**, default deny incoming; allows `22/tcp` + `60000:61000/udp` (mosh) |
| fail2ban | active, `sshd` jail only |
| unattended-upgrades | enabled |
| Tailscale | **not installed** |
| Caddy / nginx / traefik | **none installed** |
| cloudflared | **not installed** |
| Docker / podman | **not installed** |
| `jod-api` / `jod-daemon` units | **not installed** — no jod units exist |
| Resources | 4 vCPU, 11 GiB RAM, 45 GB disk (50% used, 23 GB free) |

**The good news:** the starting posture is genuinely decent. Default-deny
firewall, auto-patching, one service exposed, and a real reverse-proxy decision
still unmade. Nothing here has to be undone — the SSH item is a
misconfiguration, not an architecture problem.

**The constraint worth noting:** 23 GB free, and
[`domains/infra/README.md`](../../domains/infra/README.md) records that this
disk fills from cargo build artifacts and presents as *agents going idle*. A web
app with its own build pipeline (`node_modules`, Vite builds, Docker images)
competes for that same disk. Build artifacts and a full disk are a stability
problem, not a security one — but on this box they degrade into "the agent
silently stopped", which is worth pricing in.

---

## Not all web apps are the same app

"A web app on the VPS" covers two things with very different blast radii, and
the right answer differs.

### Class A — an app that stands alone

A blog, a landing page, a tool with its own database. If it is fully
compromised, an attacker gets that app's data and a foothold in whatever user
account it runs as. Bad, bounded, recoverable.

### Class B — anything that talks to `jod-api`

This includes **`apps/web`**, the "JOD // TACTICAL" HUD. Its
`vite.config.ts` proxies `/api` and `/ws` to `http://127.0.0.1:8787` — it is a
window onto the orchestrator.

The repo is already unambiguous about what that means:

> **A credential for this API is arbitrary code execution on the box.**
> — [`docs/jod-api.md`](../../docs/jod-api.md)

So exposing the HUD is not "publishing a dashboard". It is publishing the
front-end of a remote shell. A cross-site scripting bug in a blog is an
incident; the same bug in this app is someone spawning agents as the `jod` user.

**This is the single most important distinction in this report.** If the answer
to "which app?" is the HUD, the public-internet options below are off the table
and the Tailscale path is not merely recommended, it is the design
[`deploy/README.md`](../../deploy/README.md) already argues for — including its
explicit instruction **not** to use `tailscale funnel`.

If it is a Class A app, the public options are legitimately available. Just do
not let the two share an origin: a Class A app on the same hostname as the HUD
inherits the HUD's blast radius through the browser's same-origin model.

---

## The options

Five realistic approaches, scored against what this box actually needs.

### 1. Tailscale + `tailscale serve` — private, no public URL

`tailscale serve` terminates TLS with a real `*.ts.net` certificate and forwards
to loopback. Devices join a WireGuard mesh; each is individually authorised and
individually revocable.

- **Ports opened:** none. Nothing new is reachable from `45.45.218.177`.
- **Certificates:** issued and renewed automatically; nothing to manage.
- **Auth:** device-level, before HTTP. An unauthorised device cannot reach the
  TCP port at all, so app-layer bugs are never reached by strangers.
- **Cost:** free at this scale.
- **Cost to you:** every viewer must install Tailscale and be in your tailnet.

**This is the strongest option available, and it is strongest precisely because
it refuses to be public.** If the audience is you and a handful of trusted
devices, stop reading here — nothing below improves on it.

### 2. `tailscale funnel` — public URL over the tailnet

Same daemon, but published to the open internet.

**Recommend against.** Three reasons, in order of importance: it discards the
device-authorisation property that makes option 1 good;
[`deploy/README.md`](../../deploy/README.md) already rules it out for this box
because identity headers are not injected on funnel traffic; and it is HTTPS-only
with undisclosed bandwidth limits and no custom domain on the free tier — it is
explicitly not positioned for production hosting.

If you want a public URL, option 3 is better at that job.

### 3. Cloudflare Tunnel + Access — the public-audience recommendation

`cloudflared` makes **outbound-only** connections to Cloudflare's edge; the
firewall stays fully closed inbound. Cloudflare Access can then put an
identity check *in front of* the app, so unauthenticated users never reach your
origin at all.

- **Ports opened:** none.
- **Certificates:** managed at the edge.
- **Bundled:** DDoS absorption, WAF, custom domain, global caching.
- **Auth:** optional. Public for Class A; Access-gated for anything sensitive.
- **Cost:** free tier covers this comfortably.

Two things that are easy to get wrong:

**Cloudflare terminates TLS and sees your plaintext.** Traffic is decrypted at
the edge, inspected, and re-encrypted to your origin. For a personal app that is
usually an acceptable trade for the WAF and DDoS protection you get back. For
anything you would not want a third party to hold, it is a real objection — and
the reason option 5 exists.

**Access can be bypassed by hitting the origin directly** unless you close that
path. Access is enforced per-hostname, so an attacker who learns your origin IP
can try to skip it. Two mitigations, and you want both:

- Bind the origin to `127.0.0.1` so there is no direct path to skip *to*. This
  alone defeats it, and it is why it is a rule rather than a nicety.
- **Validate the Access JWT.** Cloudflare passes it as the
  `Cf-Access-Jwt-Assertion` header; `cloudflared` can verify it before proxying.
  Verify the *signature* against Cloudflare's published keys — checking that the
  header merely exists is not validation, and Cloudflare rotates the key pair,
  so fetch the public key from the endpoint rather than hard-coding it.

### 4. Caddy or nginx + Let's Encrypt, port 443 open

The conventional answer, and the only one on this list that puts your origin on
the public internet.

- **Ports opened:** 80/443, permanently, on an IP that already absorbs ~600
  hostile SSH probes a day.
- **You now own:** certificate renewal, TLS configuration, patching the proxy,
  rate limiting, and being the first thing a DDoS reaches.

Caddy makes this pleasant — automatic HTTPS, sane defaults, small config. It is
a perfectly respectable choice and millions of sites run it. But it trades away
the one property that makes options 1 and 3 strong: with a tunnel, there is
nothing to attack; here, there is, and its safety depends on your ongoing
maintenance.

**Recommend against for now** — not because it is bad, but because nothing about
this use case requires it. Reach for it if you later need protocols a tunnel
cannot carry, or you must not depend on a third party and option 5 is too much
machinery.

### 5. Pangolin — self-hosted tunnel, if you want no third party

An open-source tunneled reverse proxy over WireGuard. Its agent, `newt`, is the
`cloudflared` equivalent and runs unprivileged in userspace. It has grown into a
full ZTNA platform with identity-aware access control.

The trade against option 3 is clean: the public entry point is **a second VPS
you own** rather than Cloudflare's edge, so no third party terminates your TLS.
The cost is that you now run, patch, and pay for that entry point — and it has
none of Cloudflare's DDoS absorption.

**Worth it only if "Cloudflare sees my plaintext" is a real objection for you.**
For a personal app it is usually over-engineering. Noted because it is the
correct answer to a question option 3 cannot answer.

---

## Recommended architecture

### If the audience is you (and this includes the Jod HUD)

```
      your laptop / phone
             │  WireGuard, device-authorised
      ┌──────▼───────────┐
      │    tailnet       │   no inbound port on 45.45.218.177
      └──────┬───────────┘
             │  tailscale serve — TLS, real *.ts.net cert
      ┌──────▼───────────┐
      │  127.0.0.1:3000  │   the web app — loopback only
      └──────────────────┘
```

`ufw` keeps default-deny, plus `allow in on tailscale0`. The app binds
`127.0.0.1` and never `0.0.0.0`. This is the architecture
[`deploy/README.md`](../../deploy/README.md) already specifies for `jod-api`, so
the HUD and the API it talks to end up behind one door rather than two.

### If the audience is the public (Class A apps only)

```
      any browser
           │  HTTPS
      ┌────▼─────────────┐
      │ Cloudflare edge  │   WAF, DDoS, TLS · Access gate if non-public
      └────┬─────────────┘
           │  outbound-only tunnel, initiated from the box
      ┌────▼─────────────┐
      │   cloudflared    │   validates Cf-Access-Jwt-Assertion
      └────┬─────────────┘
      ┌────▼─────────────┐
      │  127.0.0.1:3000  │   the web app — loopback only
      └──────────────────┘
```

`ufw` unchanged — still no inbound rule for the app. Note the tunnel and the
tailnet coexist happily; using Cloudflare for a public app does not stop you
using Tailscale for the private one.

---

## Baseline hardening, whichever option you choose

These are independent of the ingress decision. Ordered by value per unit effort.

1. **Fix SSH.** [Above](#fix-this-before-you-expose-anything). Highest value on
   the list, and unrelated to the web app.
2. **Run the app as its own unprivileged user** that owns nothing else — not
   `reljod` (SSH keys, dotfiles, the repo), not `root`, and not `jod` unless the
   app *is* Jod. A compromised app should get an account worth nothing.
3. **Bind loopback only.** Then verify the negative, which is the step people
   skip: `ss -tlnp | grep <port>` must show `127.0.0.1`, never `0.0.0.0`. A
   tunnel in front of an app that also listens publicly buys nothing.
4. **Sandbox the unit.** `deploy/jod-api.service` is a good template, and its
   comments explain which options were deliberately *not* set and why. An
   ordinary web app is a much easier case than an agent host — it can take the
   full set (`ProtectHome=yes`, `PrivateUsers=yes`, a `SystemCallFilter`) that
   `jod-api` cannot.
5. **Keep `ufw` default-deny.** Neither recommended option needs a new rule. If
   you find yourself opening 443, that is a signal you left the tunnel design,
   not a routine step.
6. **Separate origins.** Do not serve a Class A app and the HUD from one
   hostname. Same-origin means the weaker app's XSS is the stronger app's
   session.
7. **Security headers and a CSP.** Cheap, and the main defence for a Class B app
   whose front-end holds a credential that is equivalent to shell access.
8. **Log and watch.** `journalctl -u <app>`; extend fail2ban with a jail for the
   app's auth failures if it has a login.
9. **Watch the disk.** `df -h /` — 23 GB free, and a full root filesystem on
   this box presents as agents going silent rather than as an error.
10. **Back up before you start.** A provider snapshot costs little and makes
    every step above reversible.

---

## What I did not verify

Stated plainly so none of it reads as checked:

- **No configuration was changed and no package installed.** Every command in
  this report is a proposal. The SSH fix in particular has not been applied.
- **Neither recommended path was stood up**, so no throughput, latency or
  reliability numbers here are measured — the option comparison is drawn from
  vendor documentation and current write-ups, not from this box.
- **`50-cloud-init.conf` may be rewritten by cloud-init on reboot.** The `10-`
  drop-in fix wins on sort order regardless, so it survives — but I have not
  observed a reboot to confirm cloud-init's behaviour on this provider.
- **No audit of `apps/web` itself.** I read `vite.config.ts` and the README to
  classify it; I did not review its source for XSS, dependency CVEs, or how it
  would store an API token in a browser. For a Class B app that review is
  necessary before any exposure, private or not.
- **`vite preview` is a dev server**, not a production one. Serving a built
  Vite app in production means static files behind a real server; nothing here
  covers that build-and-serve pipeline.
- **The Access-bypass mitigations are documented behaviour, not tested here.**
  If you take the Cloudflare path, prove the refusal: request the origin
  directly and confirm it fails, the same way `deploy/README.md` insists on
  `curl`-ing the public IP and watching it time out. A control you have not seen
  say *no* is a control you are only assuming.

---

## The open question

**Who is supposed to load this page, and is it the Jod HUD or something new?**

I have written this to be useful either way, but the two branches genuinely
diverge — one ends in Tailscale and no public URL, the other in Cloudflare
Tunnel and a domain. Answer that and the choice collapses to a single path.

Either way, the SSH fix comes first, and is worth doing today regardless of what
you decide about the web app.
