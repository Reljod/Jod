# Category and rule reference

Every rule `pr_triage.sh` applies, what it matches, and why it earns a
place. `SKILL.md` states the doctrine; this is the lookup table for
"why did my PR get flagged".

Read one thing first: **the rules only escalate.** Nothing here can make a
PR auto-mergeable that another rule blocked. So the question to ask of a
new rule is never "is this precise?" but "is a false positive cheaper than
a miss?" — one costs a human read, the other costs an unreviewed merge.

## Categories (assigned by path)

Categories are not exclusive. A file lands in every category it matches,
and any blocking category decides the verdict.

| Category | Matches | Auto-merge |
|---|---|---|
| `security` | `auth/`, `security/`, `secrets/`, `crypto/`, `keys/`, `.env*`, `*.pem`, `*.key`, `permissions.*` | **never** |
| `gate` | `.github/`, `.githooks/`, `.claude/hooks/`, `.claude/settings*`, `.claude-plugin/`, `CODEOWNERS`, and everything under `auto-merge/` | **never** |
| `rules` | `AGENTS.md`, `CLAUDE.md`, `REVIEW.md`, `*.md` under `.agents/` or any `skills/`, `.claude/commands/`, `.claude/agents/` | yes, unless it amends the merge policy |
| `ci` | `.github/`, `.gitlab-ci.yml`, `.circleci/`, `Jenkinsfile`, `azure-pipelines.yml` | **never** |
| `data` | `migrations/`, `alembic/`, `prisma/`, `*.sql`, `schema.*` | **never** |
| `deps` | lockfiles, `package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`, `Gemfile`, `requirements*.txt` | off by default |
| `contract` | `install.sh`, `bin/`, `api/`, `routes/`, `handlers/`, `*.proto`, `openapi.*`, `*.d.ts` | off by default |
| `docs` | `*.md`, `*.mdx`, `*.rst`, `*.txt`, `docs/`, `LICENSE` | yes |
| `research` | `research/`, `notes/`, `findings/`, `experiments/`, `explorations/`, `analysis/`, `scratch/`, `*.ipynb`, `*.csv`, `*.tsv` | yes |
| `tests` | `tests/`, `spec/`, `__tests__/`, `*_test.*`, `*.test.*`, `*.spec.*`, `test_*`, `conftest.py` | yes |
| `assets` | `*.png`, `*.jpg`, `*.gif`, `*.svg`, `*.webp`, `*.ico`, `*.woff*`, `*.ttf` | yes |
| `code` | everything else | yes, under the size limits |

`security`, `gate`, `ci` and `data` are a floor: `--allow` /
`TRIAGE_AUTOMERGE_CATEGORIES` can *remove* categories from the
auto-mergeable set but can never add these four.

### `gate` vs `rules`: enforcement vs instruction

The split is the difference between what the machine **enforces** and what
it **reads**.

`gate` is enforcement: CI workflows, git hooks, tool permissions, the
plugin manifests, `CODEOWNERS`, and this skill. Change any of it and you
change which checks can run at all — including the check being asked for
an opinion. Never auto-mergeable, no exceptions.

`rules` is instruction: the charter, skills, agent definitions, commands.
Prose an agent obeys. Most edits are as inert as any other writing — a
clarified paragraph, a new skill, a fixed example — and blocking all of
them means the charter can only be improved at a human's convenience,
which mostly means it isn't.

The one rules edit that cannot be self-approved is the one that grants the
branch permission to merge itself. That is what the `self-amendment` scan
catches, and it is why the auto-merge skill's own prose sits in `gate`
rather than `rules`: its prose **is** the merge policy.

The patterns match by shape (`auto-merge/`, `skills/`, `.claude/`) rather
than by this skill's repo-relative path. Spelling the path would break the
portability rule `tests/plugin.test.sh` enforces — a plugin user's
checkout has no `.agents/`.

### Why removed lines count, but only for rules

Everywhere else the scans read added lines: a substitution is something
you *put in*. Self-amendment is the opposite — deleting `never bypass
branch protection` weakens the rules and leaves no `+` line behind. So the
self-amendment scan reads both sides of the diff, and only for files in
`rules`.

### Why `deps` and `contract` are off but not banned

Both are real risk (a lockfile bump is code you did not write; a CLI flag
is something someone else already depends on), but both are also where
teams legitimately want automation — Dependabot patch bumps, an internal
tool's own entrypoint. They live in the allowlist so a repo can opt in
deliberately, rather than in the floor where the answer is "never".

### Why prose is exempt, and when it stops being

`docs` and `research` auto-merge because **nothing executes them**. That
is a property of the content, not the directory, so the exemption is
withdrawn the moment the content could run:

- a script extension (`.sh`, `.bash`, `.zsh`, `.fish`, `.py`, `.rb`,
  `.pl`, `.ps1`, `.js`, `.mjs`, `.ts`), or
- the executable mode bit (`100755` in the diff)

reclassifies the file as `code`, wherever it sits. `research/analyze.sh`
is a program that lives near some notes.

## Findings (assigned by content)

Scanned over **added** lines only. Lockfiles are excluded — they already
block via `deps`, and grepping a 30k-line lock for "token" is pure noise.

| Finding | Fires on | Why |
|---|---|---|
| `substitution` | `@pytest.mark.skip`/`xfail`, `#[ignore]`, `t.Skip(`, `xit(`, `.only(`, `.skip(` | A skipped test is a check narrowed to what already passes |
| `substitution` | bare `except:`, `except Exception`, empty `catch {}` | A swallowed failure is a check that cannot fail |
| `substitution` | `# noqa`, `# type: ignore`, `@ts-ignore`, `eslint-disable`, `#[allow(`, `--no-verify`, `continue-on-error: true` | A silenced check is not a satisfied one |
| `substitution` | `MagicMock`, `unittest.mock`, `@patch(`, `jest.mock(`, `sinon.stub(`, `mockito` **in non-test files** | A mock in a test is the point; in shipped code it is a swapped integration |
| `substitution` | deleted test files | The substitution a diff makes hardest to notice |
| `security` | `key`/`secret`/`password`/`token`/`credential` assigned an 8+ char literal | A credential that was invented rather than read from the environment |
| `destructive` | `rm -rf /`, `~`, `$HOME` or a bare glob; `sudo`; `curl … \| sh`; `git push --force`; `git reset --hard`; `DROP`/`TRUNCATE`/`DELETE FROM`; `dd if=`; `mkfs`; `shred`; `chmod 777`; `terraform destroy`; `kubectl delete`; `aws s3 rm`; `docker system prune` | The line between "inert change" and "this does something when it lands" |
| `self-amendment` | merge-policy language (`auto-merge`, `human-review`, `--allow`, `max-files`, `branch protection`, `--no-verify`, `--admin`, `bypass`, …) **added or removed** in a `rules` file | A branch granting itself permission to merge |
| `debug` | `debugger`, `dbg!(`, `binding.pry`, `pdb.set_trace(`, `breakpoint()` | Left-in breakpoints are unfinished work |
| `deletion` | any deleted non-doc file | Removal is the change a diff shows least legibly |
| `binary` | binary files outside the `assets` extensions | Unreviewable as text, so never unattended |
| `blocked` | `BLOCKED.md` in the diff | A successful ending per `AGENTS.md`, and a human's cue |
| `empty` | no changed files in the range | Nothing to have an opinion about |
| `size` | over `--max-files` (20) or `--max-lines` (400) **code** files/lines | Roughly where careful review stops being possible |

### Why `rm -rf` targets are narrow

The pattern matches only `/`, `~`, `$HOME` and bare globs. `rm -rf
"$tmpdir"` appears in the cleanup trap of essentially every test fixture
in existence, and a rule that fires on all of them gets the whole gate
switched off within a week. A rule nobody keeps enabled protects nothing.

### Why size counts code, not lines

Prose and assets are excluded from both limits. Volume is not risk when
nothing runs — a long research writeup would otherwise be permanently
human-review for the crime of being thorough, which is the exact opposite
of what this gate is for.

## Tuning

| Knob | Default | Effect |
|---|---|---|
| `--max-files` / `TRIAGE_MAX_FILES` | 20 | Code-file limit |
| `--max-lines` / `TRIAGE_MAX_LINES` | 400 | Code-line limit |
| `--allow` / `TRIAGE_AUTOMERGE_CATEGORIES` | `docs research rules tests code assets` | Which categories may auto-merge; the four floor categories are unaffected |

To tighten a repo to prose only: `--allow "docs research assets"`.
