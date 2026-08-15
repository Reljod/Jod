# What Jod should take from DeepSeek Harness

Companion to [`README.md`](README.md), which records what `dsh` does. This file
is the opinion: what transfers, what it costs, and what to leave.

Jod and `dsh` are not competitors — `dsh` *is* a harness, Jod *supervises*
harnesses and never calls a model itself
([decisions.md](../../docs/decisions.md)). That makes `dsh` useful as a source
of seam designs rather than as a thing to imitate wholesale.

## The measurement that motivates most of this

Jod already has the right abstraction. `core/src/harness/mod.rs:512` declares:

```rust
pub trait Harness: Send {
    fn kind(&self) -> HarnessKind;
    fn args(&self, req: &SpawnRequest) -> Vec<ArgPart>;
    fn takes_system_prompt(&self) -> bool { false }
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent>;
    fn finalize(&mut self, exit_code: Option<i32>) -> AgentEvent;
}
```

Five methods, one `Box<dyn Harness>` factory at `mod.rs:96`. That is a real
capability seam, and it is the same shape `dsh` would express as a service.

But the *identity* leaks straight through it. `HarnessKind` is a closed enum of
three variants with a `const ALL: [HarnessKind; 3]`, and **39 source files
across `core`, `cli` and `api` match on its variants** — 241 references in
`cli/src/tui/mod.rs` alone, 50 in `core/src/conversation.rs`, 36 in
`core/src/orchestrator.rs`.

So the trait buys polymorphism at spawn time, and the enum spends it everywhere
else. In `dsh` terms: Jod has a Service Definition and Providers, but the
Consumers all skipped the seam and asked *which* provider they were talking to.

That is the finding. Everything below is either a way to close that gap or an
unrelated pattern worth having.

---

## Take these

### 1. Make harness identity a value, not a variant

**Pattern in `dsh`:** `id` is a mount identity separate from `name`, the
package. Nothing downstream branches on which provider is behind `ctx.fs`.

**Do in Jod:** stop the enum at the edge of `core/src/harness/`. Anything a
caller currently learns via `match kind` should become a method on `Harness` or
a capability struct the trait returns — `supports_resume()`,
`command_convention()`, `permission_modes()`, `model_list_source()`. The three
matched facts documented in `docs/harness-support.md` (extra-directory flags,
slash-command expansion, resume flag) are precisely capability queries wearing a
variant's clothes.

**Payoff:** adding a fourth harness stops being a 39-file audit. The
[harness-support.md](../../docs/harness-support.md) matrix becomes executable
instead of prose.

**Cost:** medium and incremental. The TUI's 241 references are mostly display
and menu code, which can keep an enum for rendering while the *behavioural*
matches move behind the trait. Do the behavioural ones first; they are the ones
that go wrong silently.

**Caveat, stated honestly:** a closed enum is not a mistake in Rust the way a
hardcoded branch is in TypeScript — exhaustiveness checking is a real safety
property, and it is why adding a harness today is *loud* rather than *subtly
broken*. The recommendation is to keep exhaustiveness where it encodes a genuine
product decision and remove it where it encodes a capability, not to delete the
enum on principle.

### 2. Argv and flags as data; keep parsers as code

**Pattern in `dsh`:** *"No hardcoded tunables in plugins: deployment-varying
choices are validated `Config` fields changeable from `cordis.yml`."*

**Do in Jod:** `args()` composes flags from `SpawnRequest` in Rust today, per
harness. Most of what it encodes — the resume flag's spelling, whether
`--add-dir` repeats, how a permission mode maps — is a table, and
`docs/harness-support.md` already *is* that table in Markdown. Lift it to a
checked-in `harnesses.toml` the binary validates at load.

**Explicit limit:** do **not** try to make `parse_line` configuration.
`claude.rs` is 1,418 lines because stream dialects are genuinely code. The seam
is "invocation is data, translation is a provider."

**Payoff:** a harness version bump that renames a flag becomes a data edit and a
test, not a release. That directly serves the warning already written into
`harness-support.md`: *"none of this is a stable interface, and a version bump
is exactly when a silent change lands."*

### 3. `--dump-config`, and named profiles

**Pattern in `dsh`:** layers apply bundle → profile → home → overlay, and
`dsh --profile web --dump-config` prints what actually booted.

**Do in Jod:** `docs/harness-config.md` opens by explaining that settings live in
two places and that "telling them apart is the whole of this page." A page
exists because the runtime cannot answer the question. Add `jod config --dump`
showing the resolved per-conversation settings, their source layer, and which
harness config files were *not* read.

Then add named profiles — `jod tui --profile review` stacking a harness, model,
permission ceiling and root set — instead of accumulating flags.

**Payoff:** the highest value-to-effort item on this list. `--dump-config` is a
day of work and removes a whole class of "why did it use that model" questions.
It also pairs with `--strict-mcp-config`, where Jod already deliberately refuses
to inherit ambient MCP servers — that decision is invisible today and worth
printing.

### 4. Treat the event log as the source, not a record of it

**Pattern in `dsh`:** *"Model-visible means logged. Anything that reaches a model
request must be reconstructable from the log."* Resume, fork, transcripts,
telemetry and persistence are all *derivations* of one append-only stream.

**Do in Jod:** Jod has `AgentEvent` / `AgentEnvelope` (`core/src/event.rs`) and
one SQLite file. Audit whether the stream is authoritative or merely observed —
specifically whether anything Jod prepends to a prompt (system framing folded in
for harnesses answering `takes_system_prompt() == false`) appears in the log.
Anything folded into a prompt but not logged is a run that cannot be truthfully
replayed.

**Then take session fork.** `dsh` treats forking at an event boundary as a
first-class API. For a supervisor running fleets, "branch this conversation at
the point before it went wrong" is a strong primitive, and it is nearly free
once the log is authoritative.

### 5. Prefer a spoken protocol over a scraped one, where offered

**Pattern in `dsh`:** it ships ACP and JSON-RPC agent surfaces
(`examples/acp-agent`, `examples/jsonrpc-agent`) — structured, versioned,
bidirectional.

**Do in Jod:** Jod's harness layer is ~3,861 lines, and the bulk is
line-dialect translation whose inputs are explicitly unstable. Where a harness
speaks a real protocol, prefer it to stdout scraping and let `parse_line` be the
fallback for those that do not. ACP is the credible candidate: it carries
permission requests and tool calls as messages, which is exactly the material
Jod currently reconstructs by inference.

**Cost:** large, and not urgent — but this is the item that decides whether the
harness layer keeps growing linearly with each new harness.

### 6. Credentials resolved per request, never inlined

**Pattern in `dsh`:** live process environment over an owner-only
`$DSH_HOME/.credentials.yaml`, hot-reloaded, resolved at each request — *"so no
key is inlined in this file."*

**Do in Jod:** compare against `core/src/secrets.rs`. The three properties worth
matching are the ordering (env wins over file), the file mode (owner-only), and
resolution timing (per request, so rotation lands without a restart). Jod runs
resident on a VPS, where "restart to pick up a key" is a genuine outage.

### 7. Runnable examples as the test corpus

**Pattern in `dsh`:** *"every non-trivial model- or product-user-visible
behavior change adds or updates a keyless snapshot through a real runnable
example in the same PR"* — and `examples/` is simultaneously the documentation
and the fixtures.

**Do in Jod:** the charter already says every task needs one runnable check. The
addition is making the *example* the check. `docs/harness-support.md` is already
written as reproducible commands with quoted output; turning that file into
executable snapshots would make the re-measurement it asks for automatic rather
than remembered.

This is the cheapest cultural item and the one most aligned with what the repo
already believes.

### 8. Free-standing discovery convention

`dsh` uses a GitHub topic (`dsh-plugin`) — no registry to run. Jod already ships
a marketplace manifest; a documented `jod-skill` topic costs one line and gives
third-party skills a discovery path without operating an index.

---

## Leave these

- **"No privileged core."** Jod's value is an opinionated chief of staff, not an
  agent factory. The headless `dsh` example spends ~25 plugin rows to reach one
  agent that can edit files. Jod should keep strong defaults and expose seams at
  the few places that genuinely vary — harness, model, permission, roots.
- **Runtime plugin mounting.** Cordis's temporal composability (mount/unmount
  live) buys a Creator mode. Jod is a supervisor whose reliability story is
  static, verifiable composition; hot-swapping a running supervisor's internals
  is a liability, not a feature.
- **Whole-config replacement patching.** Blunt enough that `dsh`'s own examples
  restate unrelated fields to work around it. If Jod adds profile layering,
  merge semantics should be defined up front — and `--dump-config` is what makes
  either choice safe.
- **Vendoring a kernel.** `dsh` owns a copy of Cordis plus a sync procedure.
  Nothing here needs that.

## Suggested order

1. `jod config --dump` — a day, immediate clarity, no architectural commitment.
2. Audit the event log for prompt-visible-but-unlogged material — correctness,
   not features.
3. Move *behavioural* `HarnessKind` matches behind `Harness` capability methods,
   leaving display matches alone.
4. Lift the `harness-support.md` matrix into validated data plus snapshot tests.
5. Named profiles.
6. Evaluate ACP as a transport — a spike against `dsh` itself, which is a
   convenient ACP-speaking harness to test against.

Items 1–4 are worth doing regardless of whether Jod ever adds a fourth harness.
Item 6 is the one to revisit when it does.

## A practical aside

Adding `dsh` as Jod's fourth harness is plausible — it has a CLI with named
profiles and headless jobs, plus ACP and JSON-RPC surfaces. It would also be an
honest test of item 1: if adding it touches 39 files, the seam is still open.
Worth deferring until `dsh` leaves developer preview, given the README's
promised breaking changes.
