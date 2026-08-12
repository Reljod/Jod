# BLOCKED — `tests/e2e/harness_parity.sh` cannot report a pass

The script is written, syntax-clean, and drives real harnesses with no fakes.
Two separate things stop it reporting green, and neither is in a file this lane
owns. The first is temporary; the second is a missing feature that E3's headline
check depends on.

## Missing

**1. `HEAD` does not compile, so no binaries can be staged.**

`tests/e2e/jod/build.sh` builds a clean export of `HEAD` on purpose — the
working tree is shared by five agents, and a suite that reads `target/` reports
whatever half-state the tree was in. That export fails:

```
error[E0063]: missing field `command` in initializer of `SpawnRequest`
   --> core/src/harness/mod.rs:342:9
    |
342 |         SpawnRequest {
    |         ^^^^^^^^^^^^ missing `command`
```

`SpawnRequest` gained `pub command: Option<String>` (`core/src/harness/mod.rs:305`)
at `d0dc05f` without `impl Default for SpawnRequest` being updated. The working
tree already carries the fix (`command: None`) uncommitted, so this clears the
moment its owner commits. Not this lane's file.

**2. Nothing in Jod ever populates `SpawnRequest.secrets` or
`SpawnRequest.roots`, so no run can be given a secret.**

This is the real blocker. The supervisor half of E3 works and is proved by
`supervisor/tests/secrets_never_reach_the_record.rs`, which builds a `SpawnPlan`
directly. But no production caller fills the field that plan copies from:

| construction site | what it sets |
|---|---|
| `cli/src/main.rs:1290` (`jod run`) | `..SpawnRequest::default()` — `secrets: []`, `roots: []` |
| `api/src/routes.rs:368` (`POST /agents`) | `..SpawnRequest::default()` |
| `api/src/webhook.rs:386` | `..SpawnRequest::default()` |
| `core/src/orchestrator.rs:950` (`prepare_work`) | `..SpawnRequest::default()` |

`prepare_work` is the sharpest case: it already calls
`store.secrets_for(Some(&conversation.id), Some(&work.id))` and `store.roots(…)`
— and uses both **only to write the preamble prose**. The agent is told a
variable exists and the variable is then never injected.

The consequence for the check SPECS.md names: `plan.secrets` is always empty, so
`supervisor::inject` resolves nothing, the `Scrubber` is always empty, and a run
told to print a secret prints an empty string. "The value appears nowhere in the
database" passes trivially and proves nothing, because no value was ever put
anywhere near it. The same gap means a conversation's roots never reach a
harness: `jod root add` stores them, `--add-dir` never sees them.

## Tried

- Every construction site of `SpawnRequest` in `core/`, `cli/` and `api/`,
  by grep for `SpawnRequest {` and for `req.secrets` / `req.roots`. The fields
  are consumed (`core/src/runner.rs:143`, `core/src/harness/claude.rs:63`,
  `core/src/harness/agy.rs:77`) and assigned nowhere outside unit tests.
- The HTTP API as an alternative producer — `POST /agents` builds its request
  from a body that has no field for either, so it cannot express one.
- `open_work` as an alternative path, since `prepare_work` already computes
  both values. It discards them into prose.
- Staging binaries from `HEAD` (`tests/e2e/jod/build.sh`), which fails as above.

Not tried, deliberately: seeding `secrets` into a plan by hand, or asserting
only the half that already passes. Both would make the suite report a promise
Jod does not currently keep.

## Needs

One of these, from whoever owns the spawn path — it is two lines in the place
that already has the data:

```rust
// core/src/orchestrator.rs, in prepare_work, where `roots` and `secrets`
// are already in scope for the preamble:
let request = SpawnRequest {
    …
    roots: roots.iter().map(|r| PathBuf::from(&r.path)).collect(),
    secrets: secrets.iter().map(|s| s.name.clone()).collect(),
    ..SpawnRequest::default()
};
```

and the equivalent for `jod run`, which should fold in the conversation's roots
and in-scope secret names the way `prefer_conversation_settings` already folds
in its model and permission.

Plus a commit of the `command: None` fix so `HEAD` builds.

## Failing suite path

```
tests/e2e/harness_parity.sh
```

Specifically these checks, which are written to fail loudly rather than be
skipped:

- `[<harness>] the secret was injected into the run`
- `[<harness>] printing the secret produced the redaction marker`

Everything else in that script — two roots stored, the file in the second root
reaching the agent, a decision card, a blocking question answered from the CLI
and quoted back mid-turn, a secret card raised by name with no value, the
harness-config fingerprint, and the leak count — exercises code that exists
today and is expected to pass.
