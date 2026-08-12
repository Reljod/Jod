---
description: Wire Jod's Telegram bridge end to end — take the BotFather token, prove it, build the allowlist, start the bridge.
argument-hint: "[directory the bridge will run in, defaults to the current one]"
---

Set up the Telegram bridge — `jod telegram serve`, the inbound half of
Pillar 8. Two secrets make it work and Jod reads both from the environment:
`JOD_TELEGRAM_TOKEN` (the bot) and `JOD_TELEGRAM_ALLOWED_USERS` (who may
talk to it). This command gets both right in one pass.

Target directory: $ARGUMENTS (if empty, the current directory). It matters:
`jod` loads `.env` from its **process cwd**, so the bridge must be started
from the directory this writes.

## Handling the token

The token is a bearer credential — anyone holding it *is* the bot. So:

- **Never put it in a Bash command.** Not `curl .../bot<token>/getMe`, not
  `echo`, not `export`. It would land in shell history, in `ps`, and in this
  session's tool log. Write it to `.env` with the **Write/Edit tool** only.
- **Never echo it back** to the user, into a commit, a PR body, or `BLOCKED.md`.
  If you must refer to it, say "the token". Never `cat .env`.
- If the user pastes a token into the chat, use it and say plainly that it is
  now in the transcript and they should revoke-and-reissue via BotFather
  (`/revoke`) if that bothers them.

## Steps

1. **Preflight.** `jod telegram --help`. It must list `serve` and `whoami`.
   If `jod` is missing, or is an older build without `telegram`, install or
   update it — `install.sh` is the one way `jod` gets onto a machine:

   ```sh
   jod update    # already installed: take the newest patch
   curl -fsSL https://raw.githubusercontent.com/Reljod/Jod/main/install.sh | bash
   ```

   From a checkout you are developing in, `cargo build --release -p jod-cli`
   and run `./target/release/jod` directly rather than installing over the
   binary the box is running.

   Then `git check-ignore -q .env && echo ignored` in the target directory.
   If `.env` is *not* ignored, add it to `.gitignore` before writing anything.

2. **Get the token.** Ask the user for it. If they do not have one yet, give
   them the BotFather path verbatim: message **@BotFather** on Telegram →
   `/newbot` → pick a display name → pick a username ending in `bot` → it
   replies with a token shaped `8752043386:AAF…`. Wait for them; do not
   invent, guess, or stub a value — a fake token is exactly the
   work-around-a-blocked-check the charter forbids. No token, no setup.

3. **Write `.env`.** With the Write/Edit tool, in the target directory:

   ```
   JOD_TELEGRAM_TOKEN=<the token>
   ```

   Preserve any lines already there. Then `chmod 600 .env`. Note for the
   user: a `JOD_TELEGRAM_TOKEN` exported in their shell **beats** the file,
   deliberately — if the next step fails with a token error, that is the
   first thing to check.

4. **Prove the token.** `jod telegram whoami`. Success prints *"the token
   works — Telegram answered"*. Two failures are worth naming rather than
   retrying:
   - **409 / another poller holds this token** — a `serve` is already running
     somewhere on this same bot, most likely on the VPS. One token, one
     poller. Stop that one first, or use a second bot.
   - **401/403 / refused the token** — the token is wrong or revoked. Back to
     step 2; do not loop on it.

5. **Build the allowlist.** `whoami` reports every user who has messaged the
   bot, because an empty allowlist refuses everyone and each refusal carries
   the numeric id you need. Ask the user to open the bot in Telegram and send
   it anything, then re-run `jod telegram whoami` and copy the ids it prints.
   Add to `.env` with the Write/Edit tool:

   ```
   JOD_TELEGRAM_ALLOWED_USERS=<id>[,<id>…]
   ```

   Tell them up front that **`whoami` acknowledges those updates** — the
   messages it counted will not be redelivered to `serve`. That is expected,
   not a broken bot. `serve` refuses to start on an empty allowlist, for the
   same reason: a bot that answers nobody looks exactly like a bad token.

6. **Start the bridge.** From the target directory:

   ```
   jod telegram serve                 # add --cwd <dir> to place agent runs
                                      # add -H opencode|agy for another harness
   ```

   It prints *"listening as a … bridge in …"* and runs until Ctrl-C. Have the
   user send the bot one real message and confirm a reply comes back — that
   round trip, not the startup line, is the evidence this worked.

   Then confirm the *other* half, which is the point of the bridge: `jod main`
   in a terminal shows that message as a turn in the main chat. A message from
   the phone is not a thread of its own — it is a turn at the same desk `jod
   main` and the TUI sit at, resumed from the same session.
   → [why](../../docs/decisions.md#the-phone-types-into-the-main-chat-not-into-a-chat-of-its-own)

7. **Report.** What was written (say `.env` and the variable *names*, never
   the values), which ids are allowed, the harness and cwd the bridge uses,
   and the exact command to start it again.

   Say two things plainly, because both surprise people. An allowlisted
   message runs with Jod's orchestrator tools and in `accept-edits` — the
   allowlist is the only gate, so every id on it can drive the main chat. And
   `/new` from the phone starts the main chat over *everywhere*, not just in
   that chat; it drops the harness session, never the transcript.

## Making it resident

`serve` in a terminal dies with the terminal. On the VPS, run it in the
resident `jod` tmux console, or give it a systemd unit modelled on
`deploy/jod-daemon.service` — with `WorkingDirectory=` set to the directory
holding `.env`, or an `EnvironmentFile=` carrying the same two variables
(mode `600`, owned by the `jod` user). Do not commit either file.
