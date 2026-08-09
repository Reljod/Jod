# Jod for iPhone

`jod tui` in your pocket: one conversation, threaded across turns, with the
agents panel behind it — wearing the desktop client's face instead of a
terminal's.

```
npm install
npm run dev            # http://localhost:5174, proxying /v1 to the daemon
npm run check          # tsc --noEmit && vitest run   (127 tests)
```

Point it at a real orchestrator with `JOD_API_ORIGIN=http://127.0.0.1:8787`, or
at the box over the tailnet.

## What it is

An iPhone cannot host an agent. There is no tmux on it, no `claude` binary, and
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

The transcript vocabulary, the resume cursor, the busy guard, the thinking
toggle, the agents panel and the status line are ported from
[`cli/src/tui/app.rs`](../../cli/src/tui/app.rs) — and held there by tests that
assert what the Rust suite asserts, so the two cannot drift quietly.

| TUI | Here | Same? |
|---|---|---|
| transcript: you · agent · thinking · tool · done · notice · raw | identical seven | yes |
| resume advances to the session the harness reported | identical | yes |
| refuses a second prompt while one is in flight | identical | yes |
| `Ctrl-T` thinking toggle | THINK button | yes |
| `Ctrl-A` agents panel | AGENTS bottom sheet | yes |
| `Ctrl-L` clear | clear | yes |
| status: harness · model · cost · working/ready | identical string | yes |
| scrolling up is not undone by new output | identical rule | yes |
| `Ctrl-W` / `Ctrl-U` / byte cursor | — | no, iOS has a caret |
| line-counted scrollback | native scroll view | no, same rule |
| Enter sends | Enter newlines; SEND sends | **deliberately not** |

That last row is the one considered change. On a terminal the return key is a
deliberate act; under a thumb it is an accident, and the accident starts a real
process on the box.

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

127 tests, no device and no Mac required.

```
npm run check
```

| Suite | Covers |
|---|---|
| `session.test.ts` (45) | the reducer, against `cli/src/tui/app.rs`'s behaviour |
| `client.test.ts` (27) | the wire contract: cursors, framing, problem docs |
| `conversation.test.ts` (41) | the rules — sending, threading, recovery, auth |
| `app.test.tsx` (14) | the screen, rendered in jsdom |

Everything that can be got wrong lives in three platform-free modules —
`session.ts` (a pure reducer), `client.ts` (transport, with `fetch` and
`EventSource` injected) and `conversation.ts` (an observable store with no React
in it). The components are a projection of that state, which is why most of the
app's rules can be driven headless; `app.test.tsx` then checks that they reach
the glass.

`tests/test.sh` is the CI-discoverable entry point, so this suite runs on every
push rather than on whoever remembered to type `npm test`.

### And 27 more in WebKit, the engine iOS actually uses

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

**This is the one step that cannot happen on the VPS or in CI, and has not been
run.** An `.ipa` needs Xcode, which exists only on macOS.

The boundary is exact rather than vague, so nobody has to rediscover it:

| Checked on Linux | Result |
|---|---|
| `tauri info` parses `tauri.conf.json` | ✓ resolves dist, dev URL, CSP, React |
| iOS dependency graph (`cargo tree --target aarch64-apple-ios`) | ✓ 796 crates, **zero** gtk/glib/soup, the Apple `objc2-*`/UIKit family present |
| `cargo check --target aarch64-apple-ios` | ✗ stops at **one** crate |

The one crate is `objc2-exception-helper`, whose build script compiles a small
Objective-C shim and therefore needs `clang` plus the iOS SDK, located through
`xcrun`. That SDK ships only inside Xcode, so this is a licensing boundary and
not a fixable configuration problem — it cannot be worked around, and pretending
otherwise would be exactly the kind of faked check the charter forbids.

What that leaves: the graph is provably right for iOS (no Linux desktop stack
leaked in), the pure-Rust tree compiles for `aarch64-apple-ios`, and the shim,
the link step and the simulator are unexercised. Treat `src-tauri/` as a first
draft until a Mac has run it; expect the two manual steps below to be where the
time goes.

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
