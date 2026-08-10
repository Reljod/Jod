# Jod for iPhone

`jod tui` in your pocket: one conversation, threaded across turns, with the
slash commands, the live tool output, and the agents and team panels behind it —
wearing the desktop client's face instead of a terminal's.

---

## Setting it up

Three things have to be true: the daemon is running, the phone can reach it, and
the app is on the phone. In that order.

### 1. Run the daemon on the box

`jod-api` is the only thing the phone talks to. On `jod-cloud`:

```sh
ssh jod-cloud
sudo systemctl enable --now jod-api      # deploy/jod-api.service
systemctl status jod-api                 # confirm it is up
curl -s localhost:8787/v1/health         # {"status":"ok"} — needs no token
```

Issue yourself a token. `write` if you want to delegate from the phone; `read`
if you only want to watch. **Keep it somewhere you can paste from** — you enter
it on the phone once.

### 2. Make the phone able to reach it

The daemon binds loopback on purpose, so it is not on the internet. Put the
phone on the same tailnet:

```sh
# on the box, if it isn't already
tailscale status
```

Install Tailscale from the App Store, sign in with the same account, and confirm
the phone can see the box. The address you will type into the app is the
tailnet name plus the port — `jod-cloud:8787`.

> Plain `http://` over the tailnet is fine and is what the app assumes. If you
> put TLS in front of the daemon instead, type the `https://…` URL and iOS will
> be happier — see App Transport Security below.

### 3. Get the app onto the phone

This needs a Mac with Xcode once; after that the app stays installed.

```sh
git clone git@github.com:Reljod/Jod.git && cd Jod/apps/ios
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
npm install
npm run ios:init                 # generates src-tauri/gen/apple
npm run ios:dev                  # runs it on a simulator, or a plugged-in phone
```

To put it on your own phone rather than a simulator, open
`src-tauri/gen/apple/jod-ios.xcodeproj` in Xcode once, set a development team
under **Signing & Capabilities**, then `npm run ios:build`. A free Apple ID
works; the app expires after seven days and needs a rebuild, which a paid
developer account avoids.

**If the daemon is plain `http://`**, add an App Transport Security exception
for that host to `src-tauri/gen/apple/…/Info.plist` — iOS blocks cleartext by
default. Putting TLS in front of the daemon is the better fix and the reason
this is not pre-baked.

### 4. First launch

The app asks two things, once each:

1. **Where is the daemon?** — `jod-cloud:8787`. Remembered between launches.
   (The browser build never asks: it is served *by* the daemon, so it already
   knows.)
2. **A bearer token.** Exchanged immediately for a session cookie; **the token
   itself is never stored on the device**. Only its scope is remembered, so a
   relaunch is not read-only.

Then type. The turn runs on the box as its own supervised process and streams
back.

---

## Working on it

```
npm install
npm run dev            # http://localhost:5174, proxying /v1 to the daemon
npm run check          # tsc --noEmit && vitest run   (240 tests)
npm run test:e2e       # 38 more, in WebKit at an iPhone viewport
```

Point dev at a real orchestrator with `JOD_API_ORIGIN=http://127.0.0.1:8787`, or
at the box over the tailnet.

## What it is

An iPhone cannot host an agent. There is no `claude` binary on it, and
no shell to run them in — so unlike `apps/desktop`, which is a Tauri shell
calling `jod_core` in-process, **this app embeds nothing**. Every capability
comes over HTTP from `jod-api` running on the box.

That is not a compromise; it is the seam the architecture already had. The core
has no UI, so clients are interchangeable, and this one is the third:

| Client | Reaches the core by | Watches |
|---|---|---|
| `jod tui` | in-process, on the box | one conversation |
| `apps/web` | `jod-api` over the tailnet | the whole fleet |
| **`apps/ios`** | **`jod-api` over the tailnet** | **one conversation** |

## Parity with the TUI

The transcript vocabulary, the resume cursor, the busy guard, the slash
commands, the tool output, the panes and the status line are ported from
[`cli/src/tui/`](../../cli/src/tui/) — `app.rs` for the state, `command.rs` for
the commands, `mod.rs` for what each one does — and held there by tests that
assert what the Rust suite asserts, case for case, so the two cannot drift
quietly.

| TUI | Here | Same? |
|---|---|---|
| transcript: you · agent · thinking · tool · tool output · done · notice · raw | identical eight | yes |
| `Bash · cargo test`, not a bare `Bash` | identical, same key order | yes |
| tool output shown, failures always shown | identical rule | yes |
| a result with no call announced invents the call line | identical | yes |
| resume advances to the session the harness reported | identical | yes |
| the reported model is shown but never re-requested | identical | yes |
| refuses a second prompt while one is in flight | identical | yes |
| a slash command runs while an agent is busy | identical | yes |
| `/help` `/harness` `/model` `/thinking` `/details` `/new` `/sessions` `/resume` `/agents` `/team` `/clear` `/exit` | identical twelve | yes |
| completion popup, arguments completed too | identical list, tap to accept | yes |
| switching harness starts a fresh conversation, drops model and spend | identical | yes |
| `/resume` accepts a prefix of an agent id or a session id | identical | yes |
| `Ctrl-T` thinking toggle | THINK button, or `/thinking` | yes |
| `Ctrl-A` agents panel | AGENTS bottom sheet, or `/agents` | yes |
| `Ctrl-G` team panel | TEAM bottom sheet, or `/team` | yes |
| `Ctrl-L` clear | clear, or `/clear` | yes |
| status: harness · model · cost · working/ready | identical string | yes |
| scrolling up is not undone by new output | identical rule | yes |
| `Ctrl-W` / `Ctrl-U` / byte cursor | — | no, iOS has a caret |
| `Tab` / `↑↓` drive the popup | tap the row | no, same list |
| line-counted scrollback | native scroll view | no, same rule |
| `/exit` leaves the TUI | stops watching; the agent keeps running | no, same outcome |
| Enter sends | Enter newlines; SEND sends | **deliberately not** |

That last row is the one considered change. On a terminal the return key is a
deliberate act; under a thumb it is an accident, and the accident starts a real
process on the box.

Three rows say "no", and each is the same reason: the TUI's *mechanism* has no
meaning here, so the *rule* was ported instead of the keystroke. There is no
`Ctrl-W` because iOS gives a real caret and reimplementing readline on top of it
would be worse, not more faithful. There is no highlighted suggestion to move
with arrows because the finger goes straight to the row. And an app cannot quit
itself on iOS — but `/exit` was never really about quitting, it was about
leaving while the work carries on, which is exactly what it does.

### Teams, and why the sheet is read-only

`Ctrl-G` shows a cross-harness team: a lead on Claude Code with teammates on AGY
and OpenCode, coordinating through one inbox. The TUI reads that straight out of
SQLite, which a phone cannot do, so this branch adds two read-only routes to
`jod-api`:

```
GET /v1/teams            → ["crew"]
GET /v1/teams/{team}     → { team, members, tasks }
```

Both need only `read` scope. Nothing else was added, and deliberately: joining,
claiming and messaging are how a *teammate* participates, and a teammate is an
agent on the box with a process group. A phone watches the board; it does not
play on it.

Roster and board come back in **one** request, because the sheet draws them
together and a board from one moment against a roster from another is a screen
that was never true.

Point the app at a team by passing `team` when the conversation is built:

```ts
new Conversation({ client, team: "crew" });
```

With no team named, the sheet says so rather than showing an empty board — the
same rule as `jod tui --team`.

## Three things a phone breaks, and what handles them

1. **The link drops constantly.** Walking out of wifi, or locking the screen.
   The client follows `/v1/agents/{id}/stream`, which replays history and *then*
   goes live on one connection — so there is no window between reading what
   happened and starting to listen. `EventSource` reconnects itself with
   `Last-Event-ID`, and every envelope is deduped on `seq`, so a replay is
   idempotent. A transient drop is ignored rather than reported; only a stream
   that reaches `CLOSED` — which for this API means the session is gone — sends
   the user back to the gate.
2. **iOS suspends a backgrounded app,** taking its sockets. `visibilitychange`
   catches up over REST from the last event actually rendered, then re-opens the
   stream from there. A run that finished while the phone was in a pocket
   appears when it comes out.
3. **A retry can start a second agent.** Every spawn carries an
   `Idempotency-Key`, because two agents running the same task in the same
   directory is precisely the collision the charter's one-owner-per-path rule
   exists to prevent.

## Auth

Paste a bearer token once. `POST /v1/session` exchanges it for an
`HttpOnly; Secure; SameSite=Strict` cookie, and **the token is never stored on
the device** — a long-lived write credential in `localStorage` is a credential
available to anything that ever runs on the page, and on this client that
credential starts processes on the box.

Scope is obeyed rather than discovered: anything that is not explicitly
`"write"` is treated as read, so a read token disables the composer instead of
firing a request that 403s.

The cookie outlives the app, but nothing in JavaScript can read it — so the
*scope alone* is remembered across launches, otherwise every relaunch would be
read-only until you pasted a token again. A scope is not a credential: it grants
nothing, and the daemon re-checks it on every request. If the remembered value
is wrong the spawn comes back 403, and the app corrects the scope in place and
shows the daemon's reason rather than sending you back to the gate for a token
that was already correct.

## Tests

240 tests, no device and no Mac required.

```
npm run check
```

| Suite | Covers |
|---|---|
| `session.test.ts` (79) | the reducer, against `cli/src/tui/app.rs`'s behaviour |
| `conversation.test.ts` (66) | the rules — sending, threading, commands, teams, recovery, auth |
| `client.test.ts` (31) | the wire contract: cursors, framing, problem docs |
| `app.test.tsx` (28) | the screen, rendered in jsdom |
| `commands.test.ts` (24) | the parser and the completion list, case for case with `command.rs` |
| `origin.test.ts` (12) | where the daemon is, and what is not a usable address |

Everything that can be got wrong lives in three platform-free modules —
`session.ts` (a pure reducer), `client.ts` (transport, with `fetch` and
`EventSource` injected) and `conversation.ts` (an observable store with no React
in it). The components are a projection of that state, which is why most of the
app's rules can be driven headless; `app.test.tsx` then checks that they reach
the glass.

`tests/test.sh` is the CI-discoverable entry point, so this suite runs on every
push rather than on whoever remembered to type `npm test`.

### And 38 more in WebKit, the engine iOS actually uses

```
npx playwright install --with-deps webkit    # once
npm run test:e2e
```

The unit suites inject a fake `fetch` and a fake `EventSource`. That proves the
*rules* and nothing about the runtime — and a phone fails in ways a fake cannot
reproduce. So `e2e/run.mjs` builds the app, serves it from a stand-in daemon
(`e2e/daemon.mjs`), and drives it in **Playwright's WebKit at an iPhone 15 Pro
viewport**. WKWebView on iOS is WebKit, so this is the closest a Linux box gets
to a device.

What it catches that nothing above can:

- WebKit **really storing** the `HttpOnly` session cookie from `POST /v1/session`,
  and a real `EventSource` handshake carrying it;
- the bearer token being absent from `localStorage`/`sessionStorage` afterwards,
  and only `jod.scope` remaining;
- the page never scrolling sideways at 393pt;
- every input being ≥16px, below which **iOS silently zooms the page** on focus
  and leaves it panned with no way back;
- every visible control meeting Apple's 44px touch target;
- zero console or page errors in WebKit specifically.

This is deliberately **not** in `tests/test.sh`: it needs a browser download, and
the repo already keeps its expensive end-to-end suite out of the fast gate
(`tests/e2e/run.sh`). Run it before touching layout or the auth flow.

Pass `--screenshots <dir>` to capture each state; that is how the screenshots on
the PR were produced.

The ones worth knowing about assert the semantics that bite on a phone: that a
replayed event is drawn once, that a `lagged` frame re-reads from the last event
*rendered*, that resuming from the background does not restart the conversation,
that a read-only session keeps the text you typed, that a prompt typed while the
previous spawn is still in flight cannot start a second agent, and that the
resume cursor follows the session the harness reported rather than "the most
recent".

## Building for the device

This is the one step no Linux machine can do — which is why it happens on a
macOS runner instead of being left as a promise.
[`.github/workflows/ios.yml`](../../.github/workflows/ios.yml) compiles the
shell for iOS, generates the Xcode project, builds an unsigned simulator app,
and boots a simulator to launch it. It is scoped to `apps/ios/**` so unrelated
changes never pay for a macOS runner.

| Where | Check | Result |
|---|---|---|
| Linux | `tauri info` parses `tauri.conf.json` | ✓ |
| Linux | `cargo tree --target aarch64-apple-ios` | ✓ 796 crates, **zero** gtk/glib/soup |
| Linux | `cargo check --target aarch64-apple-ios` | ✗ blocked at one crate |
| **macOS CI** | `cargo check --target aarch64-apple-ios` | ✓ |
| **macOS CI** | `tauri ios init` | ✓ generates `jod-ios.xcodeproj` |
| **macOS CI** | `tauri ios build --target aarch64-sim` | ✓ produces `Jod.app` |

The crate that blocks Linux is `objc2-exception-helper`, whose build script
compiles an Objective-C shim and so needs `clang` plus the iOS SDK via `xcrun`.
That SDK ships only inside Xcode: a licensing boundary, not a fixable
configuration problem. On the runner it builds without complaint.

**What is still not covered:** a *device* build. That needs an Apple developer
certificate this repo does not hold, so CI stays simulator-bound and unsigned.
Signing is the step below that needs a human.

### What the simulator run caught immediately

The first successful launch produced a red link light and this in the status bar:

> The string did not match the expected pattern.

That is WebKit's `SyntaxError` from the URL parser, and it was the app doing
something that is correct everywhere except in a package: fetching `/v1/…`
**relative to the current origin**. In `apps/web` that is right — the daemon
serves the page. In the packaged app the page comes from `tauri://localhost`, so
a relative route is not a valid URL and nothing can ever reach the daemon.

No amount of Linux testing could have found this: the unit suites inject a fake
`fetch`, and the WebKit e2e run serves the app over `http://`, so both are in the
one deployment where the assumption holds. It took a real bundle on a real
simulator. That is the argument for keeping [`ios.yml`](../../.github/workflows/ios.yml).

The fix is [`src/origin.ts`](src/origin.ts): the app resolves a base URL before
it talks to anything, uses the current origin when served over http(s), and asks
for an address when it is not. Covered by `origin.test.ts`.

### Two things the CI cannot guess, learned the hard way

Both cost a failed run each, and are worth knowing before editing that workflow:

- **Build through `tauri ios build`, never `xcodebuild` directly.** The generated
  project's "Build Rust Code" phase shells back out to `tauri ios xcode-script`,
  which reads a state file only the *parent* Tauri command writes. Driving
  `xcodebuild` yourself panics with `failed to read missing addr file
  …-server-addr` — in `debug` *and* `release`.
- **Boot a simulator the runner already has.** `simctl list devicetypes` lists
  models with no installed runtime; creating one of those fails with
  `Could not find an available runtime for device type`.

On a Mac with Xcode and the iOS Rust targets:

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
npm install
npm run ios:init       # generates src-tauri/gen/apple — not tracked
npm run ios:dev        # simulator, or a plugged-in device
npm run ios:build      # .ipa
```

Two things `ios:init` generates that need editing by hand the first time, both
in `src-tauri/gen/apple`:

- **Signing.** Set a development team in the Xcode project. Without an Apple
  Developer account the app runs in the simulator only.
- **Cleartext HTTP.** If the daemon is reached over plain `http://` on the
  tailnet, iOS App Transport Security blocks it. Add an
  `NSAppTransportSecurity` exception for that host to `Info.plist`, or put TLS
  in front of the daemon — the second is better, and is the reason this is not
  pre-baked into the config.

The Rust crate is its own cargo workspace (`[workspace]` in
`src-tauri/Cargo.toml`) so that an iOS-only dependency tree never lands in front
of `cargo test` at the repo root.

## The API contract

Not redeclared here. `src/contract.ts` re-exports
[`apps/web/src/types.ts`](../web/src/types.ts), which is the checked mirror of
`core/src/{event,service,store}.rs`. A second hand-maintained copy would be a
shadow copy of a shadow copy, and the first time the two drifted one client
would be silently wrong about the wire format.
