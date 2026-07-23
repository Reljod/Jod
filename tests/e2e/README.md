# tests/e2e

`run.sh` scaffolds `setup-project.sh` against a wide array of fixture repos
under `fixtures/` and checks whether the resulting `AGENTS.md`/`CLAUDE.md`
are a plausible fit for each one. It's the release-time counterpart to
`tests/install.test.sh`: expensive and exploratory, so it doesn't run on
every push (see `.github/workflows/e2e.yml` / `release.yml`).

Two kinds of checks:

- **Structural** (hard assertions via `test-scenarios`' `assert.sh`): the
  scaffold completes, files land where expected, no leftover
  `{{PLACEHOLDER}}` tokens, requested skills copy cleanly. These fail the
  run.
- **Fitness** (soft, logged via the local `gap()` helper to
  `gaps-report.md`): whether the generated charter actually suits that kind
  of repo. A gap is a finding to support later, not a failure — that's the
  point of running this against a spread of project shapes.

Run locally:

```sh
tests/e2e/run.sh
```

## Adding a fixture

Drop a new directory under `fixtures/<name>/` with whatever files make that
project shape recognizable (a `package.json`, an existing `CONTRIBUTING.md`,
a `tests/` dir, ...), then add a section to `run.sh` that scaffolds it with
the preset/skills a human would actually pick, asserts the structural
basics, and `gap`s anything that doesn't fit.
