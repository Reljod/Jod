# Recommendations for Jod arising from the DeepSeek Harness research

This document accompanies [`README.md`](README.md), which records what DeepSeek
Harness does. This document states what Jod should do about it, what each change
would cost, and which ideas should be rejected.

The two programs are not competitors. DeepSeek Harness is itself a harness. Jod
supervises harnesses and never calls a model directly, as recorded in
[`docs/decisions.md`](../../docs/decisions.md). DeepSeek Harness is therefore
useful as a source of design ideas rather than as something to imitate.

## The measurement that prompts most of these recommendations

Jod already has the correct structure. The file `core/src/harness/mod.rs`
declares an interface at line 512 that every supported tool implements:

```rust
pub trait Harness: Send {
    fn kind(&self) -> HarnessKind;
    fn args(&self, req: &SpawnRequest) -> Vec<ArgPart>;
    fn takes_system_prompt(&self) -> bool { false }
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent>;
    fn finalize(&mut self, exit_code: Option<i32>) -> AgentEvent;
}
```

There are five methods, and a single function at line 96 selects an
implementation and returns it. This is a correct and useful arrangement, and it
is the same shape that DeepSeek Harness would express as a replaceable service.

The difficulty is that the identity of the tool passes straight through that
interface and is used everywhere else. `HarnessKind` is an enumeration with
three fixed values and a constant array listing all of them. **Thirty-nine
source files across the `core`, `cli` and `api` crates examine those values
directly.** The terminal interface in `cli/src/tui/mod.rs` does so 241 times,
`core/src/conversation.rs` 50 times, and `core/src/orchestrator.rs` 36 times.

The consequence is that the interface is used when a process is started and
largely ignored thereafter. Expressed in DeepSeek Harness's terms, Jod has
defined a service and written implementations of it, but the code that consumes
that service asks which implementation it is talking to instead of using the
interface.

That is the central finding. The recommendations that follow either address this
directly or describe an unrelated idea that is worth adopting on its own merits.

---

## Recommendations to adopt

### 1. Represent the identity of a harness as data rather than as a fixed value

DeepSeek Harness keeps the identity of an instance separate from the code behind
it, and nothing downstream examines which implementation is in use.

The equivalent change in Jod is to confine the enumeration to the
`core/src/harness/` directory. Wherever code currently examines which tool it is
dealing with, it should instead ask what that tool can do. Suitable methods
would include one that reports whether sessions can be resumed, one that reports
how commands are forwarded, and one that reports which permission modes are
available. The three properties recorded in the table in
[`docs/harness-support.md`](../../docs/harness-support.md) — the flag used to add
directories, whether slash commands are expanded, and the flag used to resume —
are questions about capability that are currently expressed as questions about
identity.

The benefit is that adding a fourth tool would no longer require a review of
thirty-nine files, and the table in `harness-support.md` would become something
the program executes rather than something a person reads.

The cost is moderate, and the work can be done gradually. Most of the 241
references in the terminal interface concern which label to display, and those
can continue to use the enumeration. The references that determine behaviour
should be moved first, because those are the ones that fail quietly when a tool
is added or changed.

One qualification should be stated honestly. A fixed enumeration is not the same
mistake in Rust that a hardcoded branch would be in a language without
exhaustiveness checking. Because the compiler requires every value to be
handled, adding a tool today produces compilation errors rather than silent
misbehaviour, and that is a genuine safety property. The recommendation is
therefore to keep the enumeration where it expresses a real product decision and
to remove it where it merely expresses a capability, rather than to remove it as
a matter of principle.

### 2. Express command-line arguments as data, but keep the output parsers as code

DeepSeek Harness requires that any setting which varies between deployments be a
validated configuration field rather than a value written into the code.

In Jod, the `args()` method assembles command-line flags in Rust, separately for
each tool. Most of what it encodes is a table: how the resume flag is spelled,
whether the directory flag may be repeated, and how each permission mode is
translated. The file `docs/harness-support.md` already contains that table,
written in Markdown. The recommendation is to move it into a checked-in data
file that the program validates when it starts.

This recommendation explicitly does not extend to `parse_line`. The file
`core/src/harness/claude.rs` runs to 1,418 lines because interpreting a stream
of output is genuinely a programming task rather than a matter of configuration.
The division being proposed is that how a tool is invoked becomes data, while how
its output is translated remains code.

The benefit is that when a tool renames a flag in a new version, the response is
an edit to a data file and a test, rather than a code change and a release. That
addresses a risk the repository has already written down: `harness-support.md`
warns that none of these interfaces is stable, and that a version upgrade is
exactly when a silent change is likely to arrive.

### 3. Add a command that prints the settings actually in use, and then add named profiles

DeepSeek Harness applies configuration in four layers and provides
`dsh --profile web --dump-config` so that the result can be inspected.

The document `docs/harness-config.md` opens by explaining that a setting can live
in two different places and that distinguishing between them "is the whole of
this page." That page exists because the program cannot answer the question
itself. The recommendation is to add a command that prints the settings in force
for a conversation, states which layer each one came from, and lists the
configuration files that were deliberately not read. This would also make
visible the existing decision to pass `--strict-mcp-config`, which prevents a run
from inheriting tool servers that Jod did not grant. That decision is sound but
currently invisible.

Once the resolved settings can be printed, named profiles become a safe addition.
A profile would combine a tool, a model, a permission ceiling and a set of
directories under one name, so that a common configuration can be selected
without accumulating command-line flags.

This is the recommendation with the best ratio of value to effort. Printing the
resolved settings is approximately a day of work, requires no architectural
commitment, and removes an entire category of uncertainty about why a run behaved
as it did.

### 4. Treat the event log as the definitive record

DeepSeek Harness requires that anything reaching a model request be
reconstructable from the session log, and derives resuming, branching,
transcripts, telemetry and storage from that one record.

Jod already has an event type and an envelope type in `core/src/event.rs`, and a
single SQLite file. The recommendation is to check whether the log is the
definitive record or merely an observation of one. The specific case to examine
is the framing text that Jod adds to the front of a prompt for tools whose
`takes_system_prompt()` returns false. If that text is added to a prompt but not
written to the log, then the log does not describe what the model was actually
sent, and a run cannot be replayed truthfully.

Once the log is known to be complete, branching a session at a chosen point
becomes straightforward to implement. For a program that supervises several
assistants at once, the ability to return a conversation to the point before it
went wrong and continue from there is a valuable feature, and it costs very
little on top of a complete log.

### 5. Prefer a defined protocol to reading printed output, where one is offered

DeepSeek Harness provides interfaces based on the Agent Client Protocol and on
JSON-RPC, both of which are structured, versioned and two-way.

Jod's harness directory contains approximately 3,861 lines, and most of that is
code that interprets each tool's printed output. The inputs to that code are
known to be unstable. Where a tool offers a defined protocol, Jod should use it
and reserve the line-reading code for tools that do not. The Agent Client
Protocol is the most promising candidate, because it carries permission requests
and tool calls as explicit messages, which is precisely the information Jod
currently has to infer.

This change is large and is not urgent. It is, however, the decision that
determines whether the amount of harness code continues to grow in proportion to
the number of supported tools.

### 6. Resolve credentials at the time of each request

DeepSeek Harness reads credentials from the process environment first and from an
owner-only file second, re-reads that file when it changes, and performs the
lookup at each request so that no key is written into a configuration file.

The recommendation is to compare this with `core/src/secrets.rs` and to match
three properties in particular: that the environment takes precedence over the
file, that the file is readable only by its owner, and that the lookup happens
per request. The third property matters most in practice, because Jod runs
continuously on a server, where requiring a restart in order to pick up a rotated
key means an interruption of service.

### 7. Make the runnable examples serve as the test fixtures

DeepSeek Harness requires that any change visible to a model or to a user update
a recorded snapshot produced by a real runnable example, and its `examples/`
directory functions as documentation and as test data simultaneously.

Jod's charter already requires that every task have one runnable check. The
addition being proposed is that the example itself becomes the check. The
document `docs/harness-support.md` is already written as a sequence of
reproducible commands with their output quoted, so converting it into executable
snapshots would make the re-measurement it asks for automatic rather than
something a person has to remember to do.

This is the least expensive recommendation on the list and the one that fits most
closely with what the repository already practises.

### 8. Adopt a discovery convention that requires nothing to be operated

DeepSeek Harness relies on a GitHub topic, `dsh-plugin`, so that there is no
registry to run and no submissions to approve. Jod already publishes a
marketplace manifest. Documenting a comparable topic, such as `jod-skill`, would
give third-party skills a route to discovery at the cost of a single line.

---

## Ideas that should be rejected

### The principle that there should be no privileged core

Jod's value lies in behaving like a competent chief of staff with sensible
defaults, not in being a kit for assembling assistants. The headless example in
DeepSeek Harness requires roughly twenty-five configuration entries before there
is an assistant capable of editing a file and running a command. Jod should
retain strong defaults and offer choices only at the few points that genuinely
vary, which are the tool, the model, the permission mode and the set of
directories.

### Loading and unloading plugins while the program runs

Cordis's ability to add and remove components at run time is what makes DeepSeek
Harness's Creator mode possible. Jod is a supervisor whose reliability depends on
its configuration being fixed and verifiable at the point it starts. Replacing
parts of a running supervisor would be a liability rather than a feature.

### Replacing settings wholesale when patching

This rule is coarse enough that DeepSeek Harness's own examples work around it by
restating settings that have not changed. If Jod introduces layered
configuration, the rules for combining layers should be decided in advance.
Printing the resolved settings, as recommended above, is what makes either choice
safe.

### Copying a framework into the repository

DeepSeek Harness maintains its own copy of Cordis together with a procedure for
keeping it synchronised. Nothing recommended here requires taking on a comparable
obligation.

---

## Suggested order of work

The following order is a genuine sequence, in that each step makes the next one
safer or easier to carry out.

1. Add the command that prints the settings actually in use. This takes about a
   day, provides immediate clarity, and commits to nothing architecturally.
2. Check whether any text sent to a model is missing from the event log. This is
   a question of correctness rather than a new feature.
3. Move the references to `HarnessKind` that determine behaviour behind methods
   on the `Harness` interface, leaving the references that only choose a label
   where they are.
4. Move the table in `harness-support.md` into a validated data file, and add
   snapshot tests that keep the file and the program in step.
5. Add named profiles.
6. Evaluate the Agent Client Protocol as a means of communication. DeepSeek
   Harness itself is a convenient tool to test against, since it speaks that
   protocol.

Steps one to four are worth carrying out whether or not Jod ever supports a
fourth tool. Step six should be reconsidered at the point that it does.

## A practical note on adding DeepSeek Harness as a fourth tool

Adding DeepSeek Harness to Jod is plausible. It has a command-line interface with
named profiles and one-off jobs, and it offers both the Agent Client Protocol and
JSON-RPC. Doing so would also be an honest test of the first recommendation: if
adding it requires changes to thirty-nine files, then the interface is still
being bypassed.

The work should nevertheless be deferred until DeepSeek Harness leaves developer
preview, given the warning in its README that compatibility will be broken.
