# BLOCKED — `tests/e2e/harness_parity.sh` runs, and cannot pass

The script is written and drives real harnesses with no fakes. It has been run
end to end against binaries staged from `68a5d17`: **10 passed, 17 failed**, and
every failure traces to one of three defects in the spawn path. None of them is
in a file this lane owns. The transcript is at `/tmp/parity-run2.log`.

## Missing

### 1. Every work session dies at spawn: the work's title is passed as the model

`core/src/orchestrator.rs:937`

```rust
let conversation = store.new_conversation(
    opening.harness,
    &checkout.to_string_lossy(),
    Some(&work.title),      // ← the third parameter is `model: Option<&str>`
)?;
```

`Store::new_conversation(harness, cwd, model)` takes the model third. A freshly
created work has `title = fallback_title(instruction)` — the instruction
truncated to about forty characters — so the conversation is stored with that
text as its model, `prefer_conversation_settings` copies it onto the request,
and the harness is launched with `--model "You are checking Jod's own plumbing
end to"`. Both harnesses died identically:

```
name                                       | status
You are checking Jod's own plumbing end to | failed

{"kind":"message","text":"There's an issue with the selected model (You are
 checking Jod's own plumbing end to). It may not exist or you may not have
 access to it. Run --model to pick a different model."}
{"kind":"finished","exit_code":1,"is_error":true}
```

The fix is one line — pass the model, or `None`:

```rust
let conversation = store.new_conversation(
    opening.harness,
    &checkout.to_string_lossy(),
    opening.model.as_deref(),
)?;
```

`prepare_work`'s unit tests inspect the returned `SpawnRequest` and never spawn,
which is why this survived: no work session has ever successfully started.

### 2. `jod run` and `jod team start` hand the agent no Jod tools

Both build their request with `tools: None` (`cli/src/main.rs:1346`,
`cli/src/main.rs:1801`), and `harness/claude.rs:166` writes no `--mcp-config`
without it. A run started either way has no `record_decision`, no
`ask_question`, no `request_secret` — so five of the six things SPECS.md asks
for are not merely untested on that path, they are impossible. The first version
of this script drove `jod run` and produced zero cards for exactly that reason.

`orchestrator::prepare_work` is the only construction site that sets `tools`,
which is why the script now opens a work through the main chat instead.

### 3. `jod run` still passes no roots and no secrets

Fixed for the orchestrator path since the wiring audit
(`core/src/orchestrator.rs:998-999`), still open for the CLI path
(`cli/src/main.rs:1346`, `..SpawnRequest::default()`). A conversation's roots and
in-scope secret names should fold into the request the way
`prefer_conversation_settings` already folds in its model and permission.

## Tried

- `jod run … -C --detach` as the driver. Ran, produced zero cards: no tools.
- The orchestrator path (`jod main` → `open_work`), which does grant tools and
  does now carry roots and secrets. The orchestrator obeyed exactly — it opened
  the work with the right harness and checkout, both harnesses, both times — and
  the session it started died at spawn on defect 1.
- Passing an explicit `model` through `open_work` to dodge defect 1. It cannot
  work: `prefer_conversation_settings` overwrites the request's model with the
  conversation's, and the conversation is the row holding the bad value.
- Staging from `HEAD` while `HEAD` did not compile (`SpawnRequest` gained
  `command` without its `Default` being updated). That one has since been fixed
  by its owner.

Not tried, deliberately: asserting only the half that already passes, or seeding
rows to stand in for a run. Two checks in this script are written to fail loudly
rather than be skipped, and they are failing loudly.

## Needs

The one-line fix in defect 1, which unblocks the whole driver. Then a re-run
decides defects 2 and 3 on evidence rather than on reading. Hand me
`core/src/orchestrator.rs` and I will make the change and re-run; it is one line
and the suite is already written around it.

## Failing suite path

```
bash tests/e2e/harness_parity.sh
```

Failing checks, per harness (`claude`, `opencode` — `agy` is skipped by name,
because its MCP config is derived from `$HOME` and redirecting `$HOME` would
take its credentials with it):

- `the run recorded a decision`
- `the run asked a blocking question`
- `the CLI's answer is stored on the card`
- `the agent received the answer mid-turn and quoted it`
- `the run requested a secret, by name and with no value`
- `the file in the second root reached the agent`
- `the run printed the secret line at all`
- `the secret was injected, rather than arriving unset`
- `printing the secret produced the redaction marker`

Passing today: the guards (`JOD_NO_MCP_INSTALL`, the scratch `JOD_HOME`, the
harness-config fingerprint), both roots on the session, the stored secret's
value staying out of the database, and the leak count of zero — which is
reported but, with nothing yet injected, deliberately not claimed as proof.
