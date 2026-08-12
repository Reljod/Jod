# Jod for iPhone

`jod tui` in your pocket. The TUI is now a nine-workspace console — chat, fleet,
memory, schedules, goals, webhooks, tasks, activity, team — and this app is being
brought up to it. [`SPEC.md`](SPEC.md) is the plan of record; §5 is the work
order and says what is done.

---

## Read this before you try to install it

The previous version of this file told you to point the app at
`http://jod-cloud:8787` and add an App Transport Security exception for
cleartext. **That setup cannot work**, and following it would have cost you a
Mac, an Xcode install and an afternoon. Three findings, each confirmed from
source:

1. **`api/src/session.rs:110` sets the session cookie `HttpOnly; Secure;
   SameSite=Strict`.** A `Secure` cookie is not stored over plain `http://` to a
   non-localhost host, so `POST /v1/session` would hand back a cookie the client
   drops on the floor. The flag's own comment says the author always assumed
   *"production is HTTPS via Tailscale"*.
2. **There is no CORS layer.** `api/Cargo.toml:25` enables tower-http's `cors`
   feature, but `CorsLayer` is never constructed anywhere in `api/src/` — so no
   `Access-Control-Allow-Origin` is ever sent. A compiled-in feature that is
   never used reads as "CORS is supported"; it is not.
3. **The packaged app is cross-origin.** Its assets load from
   `tauri://localhost` ([`src/origin.ts`](src/origin.ts)) and it uses plain
   webview `fetch` with `credentials: "include"`
   ([`src/client.ts:148`](src/client.ts)), with no `tauri-plugin-http` to route
   around the webview. Cross-origin + no CORS + `SameSite=Strict` = not one
   successful call.

So the working answer is **TLS, and the right auth for the shell you pick**.

> **None of the steps below have been run end to end.** The VPS currently has
> **no Tailscale installed and no `jod-api` systemd units** — `deploy/README.md`
> describes an intended end state, not what is running. They are written from the
> source and from Tailscale's documentation. What is still unverified is listed
> in [`SPEC.md`](SPEC.md) §4; each item is about a ten-minute check once the
> daemon is actually up. Treat this as a plan to follow together, not a recipe
> someone has already cooked.

---

## Path A — add to home screen (recommended)

No Mac. No Xcode. No seven-day expiry. This is the path to want.

It works because the page is served **from the same origin as the API**, which
satisfies `SameSite=Strict` and removes CORS from the picture entirely, and
because `tailscale serve` terminates TLS with a real `*.ts.net` certificate,
which makes it a secure context so the `Secure` cookie is stored and
`EventSource` works with its automatic reconnect and `Last-Event-ID` resume.

### 1. The daemon, on the box

```sh
ssh jod-cloud
sudo systemctl enable --now jod-api      # deploy/jod-api.service
curl -s localhost:8787/v1/health         # {"status":"ok"} — needs no token
```

Issue the phone **its own** token, and prefer `read` unless you intend to
delegate from it:

```sh
jod-api token issue phone --scope read
```

Per-device, because a token for this API is arbitrary code execution on the box.
A read token cannot execute anything if the phone is lost — the highest-leverage
control available here.

### 2. One origin

`tailscale serve` puts TLS in front of the daemon and gives it a real hostname:

```sh
tailscale serve --bg 8787       # → https://<host>.ts.net
```

The bundle has to come off **that same origin**. Do not reach for the obvious
path split — `/` for the bundle and `/v1` for the daemon fails twice, and both
failures are silent-looking 404s:

- `tailscale serve --set-path` **strips the mount prefix before proxying**, so
  `/v1/health` arrives at the daemon as `/health`, and every route in
  `api/src/lib.rs` is registered with a literal `/v1/`.
- A mount at `/` **overrides all other paths** on precedence.

**Use Caddy on loopback.** `tailscale serve --bg 8080` → Caddy on
`127.0.0.1:8080`, `/v1/*` → `:8787`, everything else → the built bundle. No code
change needed, and on loopback behind the tailnet it opens no port.

The alternative — having `jod-api` serve the bundle itself, one mount with
nothing stripped — is tidier, because same-origin becomes a property of the
binary rather than of whoever last ran `tailscale serve`. It needs a `ServeDir`
fallback in `api/` that does not exist. **Reljod was asked and chose the main
chat over the asset-serving**, so that route stays available but is not being
built today.

`tailscale funnel` is **not** an option: identity headers are not injected on
funnel traffic (`deploy/README.md`), and this API should not be on the public
internet at all.

### 3. The phone

Install Tailscale from the App Store and sign in with the same account — the PWA
does not remove that requirement, since the phone still has to resolve the
tailnet host. Then open `https://<host>.ts.net` in Safari and **Share → Add to
Home Screen**.

Paste the token once. It is exchanged for the session cookie and **never stored
on the device**; only its scope is remembered.

### 4. When it signs you out

Sessions live in memory, so **a daemon restart signs every browser out**. That is
deliberate for a credential that can execute code. A `401` therefore means
"exchange the token again", not "your token is wrong" — the app must not send you
back to the gate on a restart.

---

## Path B — the packaged app

Keep this for when you want a real app icon backed by a native shell rather than
Safari. It needs a Mac with Xcode once, and it needs one change this app has not
made yet.

**It cannot use the cookie.** `SameSite=Strict` blocks it cross-site, and a
`CorsLayer` would not rescue it. The only clean route is **bearer auth**, which
`api/src/session.rs:9-11` already anticipates: *"curl, the CLI and native mobile
clients never need a cookie."*

**Bearer alone is not enough, and this is the part to get right.** Bearer fixes
the *credential*, not the *origin*: a cross-origin `fetch` from the webview still
needs `Access-Control-Allow-Origin` on the response, and there is none. The
load-bearing half is **`tauri-plugin-http`**, which issues the request from Rust,
where the browser's CORS check does not apply. Bearer + `tauri-plugin-http`
works; bearer by itself stays blocked.

The cost to price in: `EventSource` cannot set an `Authorization` header — the
whole reason the cookie exists — so a bearer client hand-rolls SSE over `fetch`
and gives back the reconnect and resume `EventSource` provides free.
`packages/hud`'s `HttpTransport` already implements this (backoff capped at 15s,
recovery via the `after_seq` REST backfill plus `(agent_id, seq)` dedupe), but
**only its frame parser is tested** — the reconnect path is not, and that
package's live HTTP path has never run against a real daemon.

```sh
git clone git@github.com:Reljod/Jod.git && cd Jod/apps/ios
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
npm install
npm run ios:init                 # generates src-tauri/gen/apple
npm run ios:dev                  # simulator, or a plugged-in phone
```

For your own phone rather than a simulator, open
`src-tauri/gen/apple/jod-ios.xcodeproj` in Xcode once, set a development team
under **Signing & Capabilities**, then `npm run ios:build`. A free Apple ID
works but the app expires after seven days and needs a rebuild; a paid developer
account avoids that. With TLS via `tailscale serve` you do **not** need an ATS
exception.

---

## What it is

An iPhone cannot host an agent: there is no `claude` binary on it and no shell to
run one in. So unlike `apps/desktop`, this app **embeds nothing** — every
capability comes over HTTP from `jod-api` on the box. That is the seam the
architecture already had; the core has no UI, so clients are interchangeable.

| Client | Reaches the core by |
|---|---|
| `jod tui` | in-process, on the box |
| `apps/desktop` | `jod-api` on loopback, served from its own listener |
| **`apps/ios`** | **`jod-api` over the tailnet** |

## Parity with the TUI

The nav model is ported from
[`cli/src/tui/workspace.rs`](../../cli/src/tui/workspace.rs) rather than invented
— same names, same order, same letters and digits, so the muscle memory carries
across. [`src/workspaces.ts`](src/workspaces.ts) is that port, and
`test/workspaces.test.ts` asserts it case for case against the Rust suite so the
two cannot drift quietly.

```
1/c chat · 2/f fleet · 3/m memory · 4/s schedules · 5/g goals
6/h hooks · 7/t tasks · 8/a activity · 9/w team
```

Rules carried across, and the reasoning kept with them:

- **Chat is home.** Every other screen is somewhere you went from chat and come
  back to. On the phone workspaces push over chat, and the back gesture is `Esc`.
- **The local graph is not a destination.** It is memory's second level, reached
  from a focused node, because it means nothing without one — so it gets no tab
  and no digit.
- **A list's selection is an id, never a row index.** The fleet re-sorts as runs
  finish, and an index would move the cursor onto a different run at the moment
  one did.
- **An open-but-empty filter hides nothing**, so opening search never makes a
  list look empty.
- **No node-link graph.** `cli/src/tui/graph.rs` argues a drawing that impresses
  at twenty nodes is unusable at two hundred, in a terminal or out of one, and
  replaces it with a focus node plus a **visit stack**. A phone's push navigation
  *is* that visit stack — the one place the port gets simpler rather than harder.

Where a TUI mechanism has no meaning under a thumb, the **rule** is ported and
the mechanism replaced — the precedent set by there being no `Ctrl-W` here:

| TUI | Here |
|---|---|
| digits, `Alt-K` which-key | a tab bar, in MENU order |
| `↑↓` then `⏎` | tap the row |
| `/` filter line | a search field |
| `S` sort cycle | a control naming the current order |
| `⏎` re-centre / `Backspace` pop | push navigation |
| Enter sends | Enter newlines; **SEND** sends |

That last row is the one deliberate change, and it predates this work: on a
terminal the return key is a considered act; under a thumb it is an accident, and
the accident starts a real process on the box.

### The pinned main chat

In the TUI the fleet's top row **is** the pinned main chat, and `⏎` enters it.
The same is true here: the row is the chat Jod keeps — pinned, and it never ends.

Two deliberate choices behind it:

- **It draws before anyone has spoken.** `GET /v1/conversations/main` answers
  `{"conversation": null}` rather than a 404 until the first turn, and reading it
  does not *create* it — a GET that creates is a GET a prefetcher can fire.
- **The thread renders flat, following `head_id`.** `parent_id` is a real tree
  and moving `head_id` is how you switch branches, but branching has no honest
  gesture on a phone, and a tree drawn at 393pt is the same mistake
  `cli/src/tui/graph.rs` argues against for graphs. Flat is a choice, not an
  oversight.

A `403` naming `accept_edits` is shown in the daemon's own words rather than
softened. The main chat runs at `accept_edits` by construction — `ask` is plan
mode, and plan mode refuses the MCP calls that are the orchestrator's whole job —
so a daemon capped lower refuses, and the operator needs to know which knob that
is.

## The API contract

`src/contract.ts` re-exports the checked mirror of `core/src/{event,service,
store}.rs` rather than keeping a second hand-maintained copy, which would be a
shadow copy of a shadow copy.

The seven read-only workspaces are served by ten `GET` routes added in
`api/src/workspaces.rs` (all `Scope::Read`). They deliberately send **core
types, not the TUI's presentation rows** — no cron gloss, no
`"✓ verified 2m ago"`, no sparkline, because an English sentence about a cron
expression and a relative timestamp that is true for one second have no business
in a cache. **This app writes its own gloss**, porting the wording from
`cli/src/tui/data.rs`'s `gloss` so the phone and the terminal agree.

## Working on it

```
npm install
npm run check          # tsc --noEmit && vitest run
npm run test:e2e       # WebKit, at an iPhone viewport
```

Point dev at a real orchestrator with `JOD_API_ORIGIN=http://127.0.0.1:8787`.

Everything that can be got wrong lives in platform-free modules — `session.ts` (a
pure reducer), `workspaces.ts` (the nav model), `client.ts` (transport, with
`fetch` and `EventSource` injected) and `conversation.ts` (an observable store
with no React in it). The components are a projection of that state, which is why
most of the app's rules can be driven headless.

`tests/test.sh` is the CI-discoverable entry point, so this suite runs on every
push rather than on whoever remembered to type `npm test`.

### The WebKit suite, and why it exists separately

The unit suites inject a fake `fetch` and a fake `EventSource`, which proves the
*rules* and nothing about the runtime. `e2e/run.mjs` builds the app, serves it
from a stand-in daemon and drives it in **Playwright's WebKit at an iPhone 15 Pro
viewport** — WKWebView on iOS is WebKit, so this is the closest a Linux box gets
to a device.

```sh
npx playwright install --with-deps webkit    # once
npm run test:e2e
```

It catches what nothing above can: WebKit really storing the `HttpOnly` cookie
and a real `EventSource` handshake carrying it; the bearer token being absent
from storage afterwards; the page never scrolling sideways at 393pt; every input
being ≥16px, below which **iOS silently zooms the page** on focus and leaves it
panned with no way back; every control meeting Apple's 44px touch target.

Kept out of `tests/test.sh` because it needs a browser download, matching how the
repo already keeps `tests/e2e/run.sh` out of the fast gate. Run it before
touching layout or the auth flow. `--screenshots <dir>` captures each state.

### Building for the device

The one step no Linux machine can do, which is why
[`.github/workflows/ios.yml`](../../.github/workflows/ios.yml) does it on a macOS
runner: it compiles the shell for iOS, generates the Xcode project, builds an
unsigned simulator app and boots a simulator to launch it. Scoped to
`apps/ios/**` so unrelated changes never pay for a macOS runner.

`cargo check --target aarch64-apple-ios` cannot run on Linux — it stops at
`objc2-exception-helper`, whose build script compiles an Objective-C shim and so
needs `clang` plus the iOS SDK via `xcrun`. That SDK ships only inside Xcode: a
licensing boundary, not a fixable configuration problem.

**Still not covered:** a *device* build, which needs an Apple developer
certificate this repo does not hold. CI stays simulator-bound and unsigned.

Two things that cost a failed run each, worth knowing before editing that
workflow:

- **Build through `tauri ios build`, never `xcodebuild` directly.** The generated
  project's "Build Rust Code" phase shells back out to `tauri ios xcode-script`,
  which reads a state file only the *parent* Tauri command writes. Driving
  `xcodebuild` yourself panics with `failed to read missing addr file
  …-server-addr`, in `debug` and `release` alike.
- **Boot a simulator the runner already has.** `simctl list devicetypes` lists
  models with no installed runtime; creating one of those fails with
  `Could not find an available runtime for device type`.

The Rust crate is its own cargo workspace (`[workspace]` in
`src-tauri/Cargo.toml`) so an iOS-only dependency tree never lands in front of
`cargo test` at the repo root.

### What the first simulator run caught

A red link light and `The string did not match the expected pattern.` — WebKit's
`SyntaxError` from the URL parser. The app was fetching `/v1/…` **relative to the
current origin**, which is correct in `apps/web` (the daemon serves the page) and
impossible in a package, where the page comes from `tauri://localhost`.

No amount of Linux testing could have found it: the unit suites inject a fake
`fetch`, and the WebKit run serves over `http://`, so both are in the one
deployment where the assumption holds. The fix is
[`src/origin.ts`](src/origin.ts), covered by `origin.test.ts`.

That same `tauri://localhost` origin is what makes Path B need
`tauri-plugin-http` — the packaged app's origin is not a detail that can be
configured away.
