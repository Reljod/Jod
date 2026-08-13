# Measured harness support

What each harness actually does, established by running the real binaries on
this machine rather than by reading their documentation. Everything below is
reproducible: each row cites the command that produced it and quotes the output
it produced.

This file exists because [decision D7](../SPECS.md) makes forwarding behaviour a
*measurement* taken before the code is written. A guess here would have been
cheap to make and expensive to discover: two of the three harnesses behave
differently from what their `--help` implies.

Measured on 12 August 2026, Linux, against `claude` 2.1.228, `opencode` 1.18.15
and `agy` 1.1.12. Re-measure when a harness is upgraded — none of this is a
stable interface, and a version bump is exactly when a silent change lands.

## The matrix

| Harness | Extra directories | Slash-command expansion in print mode | Notes |
| --- | --- | --- | --- |
| Claude Code | `--add-dir`, repeatable, accumulates | **Native, in the prompt.** `/name` resolves both `.claude/commands/*.md` and `.claude/skills/*/SKILL.md` | `--add-dir` is variadic and will swallow a positional prompt that follows it |
| OpenCode | `--dir` only, exactly one — repeating it is a hard error | **Native, but only via a flag.** `/name` in the message is *not* expanded; `run --command <name>` resolves `.opencode/command/*.md` | Also loads `.agents/skills/*/SKILL.md` through its own `skill` tool |
| AGY | `--add-dir`, repeatable, accumulates | **Native, in the prompt.** `/name` resolves `.agents/skills/*/SKILL.md`; an unknown name is refused rather than treated as text | Has no repo command directory at all — its customisations are Skills and Rules |

| Harness | Resuming a session | Measured |
| --- | --- | --- |
| Claude Code | `--resume <id>` | Not measured against a mismatched directory |
| OpenCode | `--session <id>`, **and `--dir` must be the session's own project** | A mismatch **hangs for ever, silently** — see below |
| AGY | `--conversation <id>` | An unknown id silently starts fresh; see `harness/agy.rs` |

How Jod sends each one, derived from the middle column: `Discovered::invoke`
builds `/name args` for Claude Code and AGY, and `--command name` with the
arguments as the message for OpenCode. It refuses to send a command to a harness
whose convention it does not follow, because the only ways to make that work are
literal text the harness cannot resolve or pasting the body in — and the body is
what the last section deletes.

## Roots are not a sandbox

Nothing in this document should be read as a confinement claim. A directory flag
grants; withholding it does not deny. Measured directly — with **no** `--add-dir`
at all, Claude Code read two files well outside its working directory and
reported no permission denial whatsoever:

```console
$ cd /…/tmp/dirs/home
$ claude -p --output-format json --model sonnet \
    "Read the files /…/dirs/extra_a/alpha.txt and /…/dirs/extra_b/beta.txt and
     reply with both of their contents on one line separated by a space, nothing else."
RESULT= 'marker-ALPHA-7731 marker-BETA-9925'
DENIALS= []
```

One honest caveat, because it changes what this proves: this machine's
`~/.claude/settings.json` sets `"permissions": {"defaultMode": "auto"}`, so the
run was in bypass mode and the absence of denials is that setting's doing rather
than a demonstration of Claude Code's stock behaviour. What it does establish is
the thing Jod must not lie about — on a real box, configured the way this one is,
a harness reads whatever the filesystem will give it. Passing a root is a
convenience that puts the directory in the agent's context and in whatever
allowlist the harness keeps; it is not a wall, and Jod must never present it as
one.

## Extra directories, per harness

### Claude Code — repeatable, and it eats the prompt

`--add-dir` accumulates across repetitions. Both directories arrive:

```console
$ cd /…/tmp/dirs/home
$ claude -p --add-dir /…/dirs/extra_a --add-dir /…/dirs/extra_b \
    --output-format json --model sonnet \
    "Without using any tools, list every directory path you have been told you
     have access to, one per line, nothing else."
RESULT= '/home/reljod/.claude/jobs/db8d9808/tmp/dirs/home
/home/reljod/.claude/jobs/db8d9808/tmp/dirs/extra_a
/home/reljod/.claude/jobs/db8d9808/tmp/dirs/extra_b'
```

The trap is the argument *order*. `claude --help` declares
`--add-dir <directories...>` — variadic — so the flag keeps consuming words until
it meets another flag. Put the positional prompt straight after it and the prompt
becomes a directory:

```console
$ claude -p --output-format json --model sonnet \
    --add-dir /…/dirs/extra_a --add-dir /…/dirs/extra_b \
    "Reply with the word TWODIRS and nothing else."
Error: Input must be provided either through stdin or as a prompt argument when using --print
```

Jod is safe from this by accident rather than by design: `claude.rs` emits
`-p <prompt>` first, so the prompt can never trail a variadic flag. That accident
is now load-bearing, which is why there is a test pinning it — moving the prompt
to the end would reintroduce a failure whose message names neither `--add-dir`
nor the prompt.

### AGY — repeatable, and it is the only thing that sets the workspace

```console
$ cd /…/tmp/dirs/home
$ agy --print "Without using tools, list every directory in your workspace, one per line, nothing else." \
    --output-format stream-json --print-timeout 3m \
    --add-dir /…/dirs/extra_a --add-dir /…/dirs/extra_b
{"event":"result","result":{…,"status":"SUCCESS","response":"/home/reljod/.claude/jobs/db8d9808/tmp/dirs/extra_a\n/home/reljod/.claude/jobs/db8d9808/tmp/dirs/extra_b\n",…}}
```

Note what is *absent*: the shell's working directory, `dirs/home`, is not in the
list. AGY builds its workspace purely from `--add-dir` and its own settings, which
confirms the existing comment in `harness/agy.rs` — the cwd grant is not
redundant, and dropping it would leave AGY writing into its scratch directory
while reporting success.

### OpenCode — one directory, and repeating the flag is fatal

`opencode run --help` offers `--dir` and nothing else in this area. It takes a
single string, and a second one crashes the process before any model call:

```console
$ opencode run --format json --dir /…/dirs/extra_a --dir /…/dirs/extra_b \
    "Run pwd and reply with only its output."
Error: Unexpected error

The "paths[1]" property must be of type string, got array
```

**This is a documented degradation, not a bug to work around.** Under OpenCode a
conversation's extra roots do not reach the harness at all: `--dir` already
carries the working directory, and overwriting it with a root would silently move
the project the run happens in — a worse failure than not granting the root,
because it would look like it worked. So `opencode.rs` deliberately emits nothing
for `req.roots`, and Jod's preamble is where an OpenCode run learns that its other
roots exist. The agent can still read them; it simply has not been *granted* them,
and the distinction is one the earlier section says Jod must keep honest anyway.

Whether OpenCode's config file can name additional directories is **unmeasured**.
The binary contains the strings `additionalDirectories` and `workspaceFolders`,
but both appear in its bundled Agent Client Protocol and language-server code
rather than anywhere reachable from `opencode.json`, so treating them as a
supported route would be a guess. Not tested, not claimed.

## Resuming, and the OpenCode hang

A session id is not enough to resume an OpenCode session. **The session is scoped
to the project OpenCode resolves from `--dir`, and `--session <id>` naming a
session from a different project does not error and does not start fresh — it
produces no output on either stream and never exits.**

Four runs, in order, on 12 August 2026 against `opencode` 1.18.15. A fresh run in
`tmp/oc`, reporting its session id:

```console
$ opencode run --format json --dir /…/tmp/oc "Reply with exactly the word FRESHOK and nothing else."
{"type":"text",…,"sessionID":"ses_008b390a4ffe8ayOultV14MuPU",…,"text":"FRESHOK",…}
{"type":"step_finish",…,"tokens":{"total":10291,"input":8349,…}}
EXIT=0
```

Resumed in the **same** directory — works, and the token counts prove the context
came back rather than being rebuilt (`input` falls from 8349 to 68, with 10240
read from cache):

```console
$ opencode run --format json --dir /…/tmp/oc --session ses_008b390a4ffe8ayOultV14MuPU "…RESUMEDOK…"
{"type":"text",…,"text":"RESUMEDOK",…}
{"type":"step_finish",…,"tokens":{"total":10313,"input":68,…,"cache":{"read":10240}}}
EXIT=0
```

`--thinking`, which Jod always passes, changes nothing — ruled out explicitly
because it was the first suspect:

```console
$ opencode run --format json --dir /…/tmp/oc --thinking --session ses_008b390a4… "…THINKRESUME…"
{"type":"reasoning",…}
{"type":"text",…,"text":"THINKRESUME",…}
EXIT=0
```

The same session id with a **different** `--dir` — nothing, on either stream,
until it was killed at ninety seconds:

```console
$ cd /…/tmp/oc-other
$ timeout 90 opencode run --format json --dir /…/tmp/oc-other --thinking \
    --session ses_008b390a4ffe8ayOultV14MuPU "Reply with exactly the word WRONGDIR and nothing else."
Terminated
EXIT=143
```

The control that makes this the *session lookup* rather than the directory: a
**fresh** run in that same other directory is fine.

```console
$ opencode run --format json --dir /…/tmp/oc-other --thinking "…OTHERDIRFRESH…"
{"type":"text",…,"text":"OTHERDIRFRESH",…}
EXIT=0
```

### What this cost, and what Jod does about it

This is what made a resumed OpenCode run in the parity suite emit a single
`finished` event with no content and never terminate, twice, at ten and
twenty-two minutes. The session id was correct throughout; the directory was
not — `jod run -s <id>` took the directory the command was typed in.

**Jod's half is fixed and its half only.** A resume now happens in the directory
its session belongs to: `session_cwd` in `cli/src/main.rs` resolves the session
to its conversation and uses that `cwd`, and an explicit `--cwd` still wins,
because somebody who names a directory means it.

**No workaround for the hang itself, deliberately.** Jod cannot tell a session
that will never answer from a model that is thinking, so a timeout here would
have to be a guess, and a guess that fires early kills real work. What Jod can
do is stop *causing* the mismatch, which is what the fix does. A session started
outside Jod and resumed by id is still exposed — there is nothing to look up —
and the honest answer for that case is this section.

Not measured, and so not claimed: whether Claude Code and AGY behave the same way
on a mismatched directory. AGY has a documented defect of a related shape — an
unknown `--conversation` silently starts fresh, losing the thread rather than
hanging — and `harness/agy.rs` carries that measurement.

## Slash-command expansion, per harness

The probe was the same shape for all three: a scratch repository under
`$CLAUDE_JOB_DIR/tmp` containing one command file per harness convention whose
entire body is `Reply with exactly the word CMDFIRED and nothing else.`, and a
skill whose body says `SKILLFIRED`. Distinct words on purpose — an earlier run
used `BANANA` for both and produced a result that looked like a clean pass and
was not, because OpenCode had reached the answer through the skill rather than
through the command. Two paths sharing one payload cannot be told apart, and the
first version of this measurement was wrong for exactly that reason.

### Claude Code — expands both commands and skills from the prompt

```console
$ cd /…/tmp/pc          # contains .claude/commands/jodcmd.md
$ claude -p --output-format json --model sonnet "/jodcmd"
RESULT= 'CMDFIRED'

$ claude -p --output-format json --model sonnet "/jodskill"
RESULT= 'SKILLFIRED'
```

No tool calls, no searching: the command's text was resolved before the model saw
it. Forwarding the line as typed is all Jod has to do.

### AGY — expands skills from the prompt, and refuses what it does not know

```console
$ cd /…/tmp/pa          # contains .agents/skills/jodskill/SKILL.md
$ agy --print "/jodskill" --output-format stream-json --print-timeout 3m --add-dir /…/tmp/pa
{"event":"step_update",…,"tool_name":"view_file","tool_info":{"parameters":{"AbsolutePath":"/…/pa/.agents/skills/jodskill/SKILL.md"}}}
{"event":"result","result":{…,"status":"SUCCESS","response":"SKILLFIRED\n",…}}
```

AGY resolves the name and then reads the file through `view_file`, which is its
documented skill protocol rather than the model improvising — the control run
proves the syntax is genuinely parsed, because an unknown name is *rejected*
instead of being passed through as prose:

```console
$ agy --print "/jodnosuchthing" --output-format stream-json --print-timeout 2m --add-dir /…/tmp/pa
{"event":"result","result":{…,"response":"`/jodnosuchthing` is not a recognized command or skill. \n\nAvailable slash commands include:\n- `/goal`\n- `/schedule`\n- `/plan`\n- `/grill-me`\n- `/teamwork-preview`\n- `/learn`\n\nIf you meant to invoke a specific skill or action, please check the command name and try again.\n",…}}
```

That control is the reason this row says "native" rather than "the model happened
to cooperate". AGY has no repo *command* directory to scan: `agy --help` and its
own embedded documentation describe customisations as **Skills** (directories
holding a `SKILL.md`) and **Rules** (markdown files), and the binary's only
workspace-relative customisation paths are `{workspace}/.agents/skills/{name}/SKILL.md`
and `{workspace}/.agents/agents/{name}/`. Jod therefore scans `.agents/skills/`
for AGY and invents no command path — a directory nobody reads is worse than an
absent row, because it looks like coverage.

### OpenCode — not from the prompt, natively from a flag

Given `/jodcmd` in the message, OpenCode does **not** expand it. The transcript
shows the model receiving the literal text, not recognising it, and going hunting
for something that would explain it:

```console
$ cd /…/tmp/po          # contains .opencode/command/jodcmd.md
$ opencode run --format json --dir /…/tmp/po "/jodcmd"
"type":"tool","tool":"bash",…"input":{"command":"ls -la"}…
"type":"tool","tool":"bash",…"input":{"command":"ls -la .opencode work"}…
"type":"tool","tool":"bash",…"input":{"command":"ls -la .opencode/command && cat .opencode/.gitignore"}…
"type":"tool","tool":"read",…"input":{"filePath":"/…/po/.opencode/command/jodcmd.md"}…
"type":"text",…"text":"CMDFIRED"…
```

The final answer is right, which is precisely what makes this the dangerous
result to skim. It cost four tool calls and it worked only because the file
happened to sit in the working directory and the model happened to go looking.
A command stored anywhere else, or a less curious model, produces a run that
answers the literal text `/jodcmd`. This is not expansion and Jod must not count
it as such.

The flag is expansion, and it is clean — one step, no searching:

```console
$ opencode run --format json --dir /…/tmp/po --command jodcmd
"type":"text",…"text":"CMDFIRED"…
```

With the flag set, the positional message stops being a message and becomes the
command's *arguments*. A command whose body is `Reply with exactly
ARGS=[$ARGUMENTS] and nothing else.` shows where the words land:

```console
$ opencode run --format json --dir /…/tmp/po --command jodargs "hello world"
TEXT= 'ARGS=["hello world"]'
```

That is why `SpawnRequest::command` and `SpawnRequest::system` do not combine
under OpenCode. OpenCode has no system-prompt flag, so the runner prepends the
framing to the prompt — and under a command, the prompt is argument text. One or
the other, and it is written down on the field because nothing in the types
prevents it.

### A forwarded command does not have to lead the line

The obvious guess is that `/name` must be the first thing in the message. If
that were true, AGY would silently lose every command Jod forwards: it answers
`false` to `takes_system_prompt`, so `runner.rs` puts the worker preamble in
front of the prompt and the slash ends up several lines down. Measured instead
of assumed, and the guess is wrong:

```console
$ agy --print "You are a worker agent. Your roots are /tmp. Changing anything means claiming a worktree first.

---

/jodskill" --output-format stream-json --print-timeout 3m --add-dir /…/tmp/pa
{"event":"result","result":{…,"status":"SUCCESS","response":"SKILLFIRED\n",…}}
```

Claude Code takes arguments after the name in the same way, and expands
regardless:

```console
$ claude -p --output-format json --model sonnet "/jodcmd please"
RESULT= 'CMDFIRED'
```

Nothing on the argv path rewrites either line. `runner.rs` resolves the prompt
placeholder to the string as it stands and hands argv to `execve`; there is no
shell left to re-read a leading slash.

## What this measurement decided

D7 says that if all three harnesses expand their own commands, the inlining
branch is deleted rather than kept just in case. They do — every one of the three
resolves its own commands from its own files, with no help from Jod. Two read
`/name` straight out of the prompt; the third needs the name in a flag instead.
So `Expansion::Inline` and the body-substitution path it existed for are
**deleted**, and `commands.rs` carries `Prompt` and `Flag` — the two spellings
actually observed — plus `Unmeasured` for a harness nobody has run yet.

The `discovered_commands.body` column survives the deletion, empty. Dropping it
belongs to whoever owns `store.rs`, and a nullable unused column costs nothing
next to a migration written to tidy up.

What Jod does **not** do is forward a command across conventions. A
`.claude/commands/foo.md` handed to OpenCode has no `.opencode/command/foo.md` to
resolve, and inlining its body to paper over that would rebuild the branch this
measurement just deleted, for a case D7 never asked for. Each discovered command
records the harness whose convention it follows, and the palette offers it to
that harness.

## Where each harness looks

Discovery in `commands.rs` scans exactly these, and nothing on speculation.

| Path | Kind | Harness | How it was established |
| --- | --- | --- | --- |
| `<root>/.claude/commands/*.md` | command | Claude Code | Measured — `/jodcmd` returned `CMDFIRED` |
| `<root>/.claude/skills/*/SKILL.md` | skill | Claude Code | Measured — `/jodskill` returned `SKILLFIRED` |
| `~/.claude/commands/*.md` | command | Claude Code | Same convention at user scope. Does not exist on this box, so its *contents* are unmeasured; the path is scanned because Claude Code documents it |
| `<root>/.opencode/command/*.md` | command | OpenCode | Measured — `run --command jodcmd` returned `CMDFIRED`. Singular `command`, and the binary contains both spellings, so the plural is not scanned on a guess |
| `<root>/.agents/skills/*/SKILL.md` | skill | AGY, OpenCode | Measured twice — AGY resolved `/jodskill`; OpenCode loaded the same directory through its `skill` tool unprompted |

The repository's own `.agents/skills/` is the portable toolkit the charter
describes, so a Jod checkout is already a root full of skills that two of the
three harnesses read without being told.
