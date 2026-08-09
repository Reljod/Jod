# Deploying `jod-api` on the VPS

Putting this daemon on a machine puts an endpoint that **spawns agent harnesses**
on that machine. An agent harness runs shell commands, so a credential for this
API is arbitrary code execution as the `jod` user. Everything below follows from
taking that seriously. → [`docs/jod-api.md`](../docs/jod-api.md)

The box is reached as `ssh jod-cloud` — an alias in `~/.ssh/config`. This repo is
public and never spells out the address.
→ [why](../docs/decisions.md#the-vps-address-lives-in-ssh-config-not-in-the-repo)

This is **not** installed by `install.sh`. Standing up an RCE endpoint should be
a deliberate act, never a side effect of installing a CLI.

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
      jod-core ──► tmux ──► claude / opencode / agy
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

## 2. Install the binary

Build on the box (or copy a matching binary in):

```sh
sudo -u jod git clone <repo> /home/jod/src/Jod
cd /home/jod/src/Jod && cargo build --release -p jod-api
sudo install -m 0755 target/release/jod-api /usr/local/bin/jod-api
```

`tmux` is a hard requirement — every agent runs inside a session. So is at least
one harness (`claude`, `opencode`, or `agy`) on the `jod` user's `PATH`.

```sh
sudo apt install tmux
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
- **Transcripts:** `/home/jod/.jod/runs/<id>/stream.jsonl`, readable with `cat`
  whether or not the daemon is running.
- **Watch an agent directly:** `sudo -u jod tmux attach -t jod-<id>`.
- **Restart:** `sudo systemctl restart jod-api`. Running agents survive — they
  live in tmux, not as children of the daemon — and the daemon reloads prior
  runs from the store on boot. Browser sessions are dropped, which is deliberate.

## If a token leaks

```sh
sudo -u jod jod-api token revoke <label>
sudo systemctl restart jod-api      # also drops every browser session
grep '<label>' /home/jod/.jod/audit.jsonl    # what it did
sudo -u jod tmux ls                          # what is still running
```

Then read the transcripts of anything that ran. Revocation stops future use; it
does not undo what already executed.
