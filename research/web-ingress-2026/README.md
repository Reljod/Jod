# Web ingress research — exposing a web app on the VPS securely

How to serve a web app from `Jod` without widening the box's attack surface.

**→ Read [`REPORT.md`](REPORT.md).**

Companion to the [VPS study](../vps-comparison-2026/REPORT.md) (which box) and
the [IP-blocking study](../ip-blocking-2026/REPORT.md) (getting *out*). This one
is about getting *in* — the ingress path, and who is allowed to walk it.

## The short version

The answer forks on audience. **Private** (you and trusted devices) → Tailscale
+ `tailscale serve`. **Public** → Cloudflare Tunnel, with Access in front of
anything non-public. Both open **zero inbound ports**; the origin binds
`127.0.0.1` either way.

Two things outrank the choice:

- **SSH accepts password logins for `root` from the internet** — measured, with
  ~600 failed attempts a day against it. Fix before exposing anything.
- **`apps/web` is a control panel for an RCE endpoint**, not an ordinary web
  app. It does not belong on the public internet under any option here.

## Status

**Research only — nothing on the box was changed.** Every command in the report
is a proposal, including the SSH fix. Box facts were measured on 2026-08-12; the
option comparison is from vendor docs, not benchmarked here. Limits are listed
in [What I did not verify](REPORT.md#what-i-did-not-verify).
