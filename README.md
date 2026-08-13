<div align="center">

```
     ██╗ ██████╗ ██████╗
     ██║██╔═══██╗██╔══██╗
     ██║██║   ██║██║  ██║
██   ██║██║   ██║██║  ██║
╚█████╔╝╚██████╔╝██████╔╝
 ╚════╝  ╚═════╝ ╚═════╝
```

**Autonomous multi-agent orchestration and terminal agent workspace.**

*Supervise, orchestrate, and delegate tasks across agent harnesses in one unified interface.*

</div>

---

## Install

Three clients, built from the same tag by the **Build clients** workflow and attached to the [latest release](https://github.com/Reljod/Jod/releases/latest). Every download below also has a `.sha256` beside it on the release page.

> **None of these are code-signed or notarised.** Signing needs an Apple Developer certificate this repo does not hold, so each section below carries the one command your OS needs to accept an unsigned build. If you would rather not, [build from source](#jod-tui--from-source) — that path is signed by nothing because nothing is downloaded.

### Jod TUI — prebuilt

The `jod` CLI and TUI, plus the `jod-run` supervisor and the optional `jod-api` daemon. No Rust toolchain needed.

```sh
# Pick your platform
#   macOS, Apple Silicon   aarch64-apple-darwin
#   macOS, Intel           x86_64-apple-darwin
#   Linux, x86_64          x86_64-unknown-linux-gnu
#   Linux, ARM64           aarch64-unknown-linux-gnu
TARGET=aarch64-apple-darwin

curl -fsSL -o jod.tar.gz \
  "https://github.com/Reljod/Jod/releases/latest/download/jod-$TARGET.tar.gz"
mkdir -p ~/.local/bin && tar -xzf jod.tar.gz -C ~/.local/bin
# macOS only — clear the quarantine flag on an unsigned download
xattr -dr com.apple.quarantine \
  ~/.local/bin/jod ~/.local/bin/jod-run ~/.local/bin/jod-api 2>/dev/null || true

jod --version && jod tui
```

Make sure `~/.local/bin` is on your `PATH`.

### Jod TUI — from source

The original path, and still the one `jod update` drives. Requires Git and [Rust](https://rustup.rs):

```sh
curl -fsSL https://raw.githubusercontent.com/Reljod/Jod/main/install.sh | bash
```

The installer clones into `$HOME/.jod/src` (or `$JOD_SRC`), compiles `jod` and `jod-run`, and installs them into `$HOME/.local/bin` (or `$JOD_BIN_DIR`). Add the optional HTTP daemon with `JOD_WITH_API=1`:

```sh
curl -fsSL https://raw.githubusercontent.com/Reljod/Jod/main/install.sh | JOD_WITH_API=1 bash
```

See [`deploy/README.md`](./deploy/README.md) for daemon and VPS deployment. Once installed, `jod update` takes newer patch releases within your installed `MAJOR.MINOR`.

### Jod Desktop

A window onto a locally-served `jod-api`. **macOS** — one universal `.dmg` covering Apple Silicon and Intel:

```sh
curl -fsSL -o jod-desktop.dmg \
  "https://github.com/Reljod/Jod/releases/latest/download/jod-desktop-macos-universal.dmg"
open jod-desktop.dmg     # drag Jod.app to /Applications, then:
xattr -dr com.apple.quarantine /Applications/Jod.app
```

Without that `xattr` line, macOS reports the app as damaged — that is Gatekeeper refusing an unsigned bundle, not a broken download.

**Linux** — `.deb` on Debian/Ubuntu, or the `.AppImage` anywhere:

```sh
# Debian / Ubuntu
curl -fsSL -o jod-desktop.deb \
  "https://github.com/Reljod/Jod/releases/latest/download/jod-desktop-linux-x86_64.deb"
sudo apt install ./jod-desktop.deb

# Or, distro-independent
curl -fsSL -o Jod.AppImage \
  "https://github.com/Reljod/Jod/releases/latest/download/jod-desktop-linux-x86_64.AppImage"
chmod +x Jod.AppImage && ./Jod.AppImage
```

Needs a WebKit runtime (`libwebkit2gtk-4.1-0`); the `.deb` pulls it in itself.

### Jod iOS

**Simulator only.** Installing on a physical device requires an Apple Developer certificate this repo does not hold, so no `.ipa` is published — [build it yourself](./apps/ios) with your own signing identity if you need one. For the simulator, on a Mac with Xcode:

```sh
curl -fsSL -o jod-ios.zip \
  "https://github.com/Reljod/Jod/releases/latest/download/jod-ios-simulator.zip"
unzip -q jod-ios.zip

xcrun simctl boot "iPhone 16" || true      # any booted simulator works
xcrun simctl install booted Jod.app
xcrun simctl launch booted dev.reljod.jod
```

### Cutting a release

Two steps, and the split is deliberate — deciding a version and shipping binaries are different acts:

```sh
# 1. Tag it. Runs the suite, tags, creates the GitHub release.
gh workflow run release.yml --ref main -f version=v0.2.0

# 2. Build the three clients and attach them to that release.
gh workflow run build-clients.yml --ref main -f version=v0.2.0
```

Run step 2 with the version left **blank** to build all three clients from `main` without publishing anything — the results stay on the workflow run as artifacts. Individual clients can be switched off with the `tui`, `desktop` and `ios` toggles. See [`.github/workflows/build-clients.yml`](./.github/workflows/build-clients.yml).

---

## Overview

**Jod** is a standing multi-agent orchestrator built in Rust. It lives on your machine or VPS, supervising agent runs across supported harnesses (**Claude Code**, **OpenCode**, and **AGY**) and persisting every event, run, and memory into a single SQLite store.

Jod does not generate code itself - it delegates tasks to isolated agent harnesses running under process supervision (`jod-run`), normalizes their output into a unified event stream, and lets you manage, watch, and interact with all agents concurrently.

```mermaid
flowchart LR
    User(["User / TUI / CLI"])
    JodCore["Jod Core\n(Process supervision & SQLite store)"]

    User --> JodCore

    JodCore --> H1["Claude Code"]
    JodCore --> H2["OpenCode"]
    JodCore --> H3["AGY"]

    H1 --> Events["Unified Event Stream\n(SQLite)"]
    H2 --> Events
    H3 --> Events

    style JodCore fill:#4f46e5,stroke:#3730a3,color:#fff
    style Events fill:#059669,stroke:#047857,color:#fff
```

---

## Jod TUI (`jod tui`)

The centerpiece of Jod is `jod tui`: a rich, full-screen terminal interface designed for multi-agent delegation and monitoring.

```sh
jod tui
```

### Key Features of Jod TUI

- **Pinned Orchestrator Chat (`jod main`)**: The top row of the fleet view is permanently reserved for `jod main`. It is Jod's own orchestrator conversation that remains available across sessions. Pressing `Enter` on the top row enters the orchestrator chat, where you can issue meta-level instructions (delegation, scheduling, goal setting, agent inspection).
- **Fleet View (`Ctrl-A`)**: Toggle the fleet management panel listing every active and historical agent process group.
- **Background Delegation (`Ctrl-B`)**: Instantly delegate a prompt to a detached background agent process that runs independently and reports back upon completion.
- **Agent Interactivity**: Select any running or finished agent run from the panel to attach, view live transcripts, pause, resume, or terminate execution.
- **In-TUI Live Updates (`/update`)**: Trigger background binary updates from within the console. Build progress streams into the transcript, allowing you to re-exec into updated binaries without dropping your terminal session.

### Pinned Orchestrator Chat (`jod main`)

`jod main` is the single persistent orchestrator conversation. Rather than answering prompts directly, it uses Model Context Protocol (MCP) tools to act on your workspace:

| You say | Jod Main executes | Result |
|---|---|---|
| *"Every weekday at 8am, sweep open PRs"* | `schedule_create` | Armed recurring cron schedule |
| *"Keep refactoring until tests pass"* | `goal_create` | Persistent goal loop with automated checks |
| *"Build feature X"* | `delegate` | Spawns a new supervised agent run |
| *"Continue on that error"* | `continue_agent` | Resumes existing agent with full context |

---

## Core Features & Architecture

Jod is structured into decoupled Rust components:

| Component | Path | Function |
|---|---|---|
| **Core Engine** | [`core/`](./core) | Harness abstractions, SQLite event logging, memory management, and MCP tools |
| **Supervisor** | [`supervisor/`](./supervisor) | `jod-run`: Process group supervisor managing agent execution and stdout/stderr normalization |
| **CLI & TUI** | [`cli/`](./cli) | `jod` binary offering terminal commands and full-screen TUI interface |
| **HTTP Daemon** | [`api/`](./api) | `jod-api`: Optional REST daemon providing remote access to the SQLite event store |

### Unified Event Stream & Process Supervision

Every delegated run is executed in its own detached process group managed by `jod-run`. If the TUI exits or your terminal disconnects, background agent tasks continue uninterrupted. Every tool call, prompt turn, stdout line, and exit state is recorded in `$HOME/.jod/jod.sqlite`.

---

## CLI Usage

```sh
# Start the full-screen terminal interface
jod tui

# Talk directly to the pinned orchestrator conversation
jod main "check status of current background runs"

# Delegate a single prompt directly to an agent harness
jod run "summarize git changes in this directory"

# Start a simple terminal chat
jod chat

# Query Jod memory and past run context
jod recall "what were the test results for the last PR?"

# Check available agent harnesses on your system
jod harnesses

# Update Jod binaries to the latest patch release
jod update
```

---

## Portable Skills Toolkit & Plugin

Jod includes a reusable, project-agnostic toolkit under [`.agents/skills/`](./.agents/skills) for workflow automation (PR creation, spec writing, TDD loops, scenario testing, and git hooks).

### Installing as a Claude Code Plugin

Register the catalog and install the plugin:

```sh
/plugin marketplace add Reljod/Jod
/plugin install jod@reljod
/reload-plugins
```

This brings slash commands directly into your Claude Code workflow:
- `/jod:write-spec` - Interactive spec generator prior to implementation
- `/jod:create-pr` - Rich PR description generator with visual deltas
- `/jod:tdd-loop` - Red-green-refactor TDD watch loop
- `/jod:test-scenarios` - Exhaustive edge-case test coverage auditing
- `/jod:setup-git-hooks` - Deterministic pre-commit/pre-push git hooks installer
- `/jod:setup-project` - Interactive repository charter & skills bootstrapper

---

## Repository Layout

```
AGENTS.md          Charter, coding conventions, and agent rules
CLAUDE.md          Symlink to AGENTS.md for Claude integration
install.sh         Bootstrap installer script for Jod
core/              Rust core: process supervision, SQLite store, events, MCP tools
supervisor/        jod-run supervisor daemon
cli/               jod CLI and full-screen TUI implementation
api/               jod-api HTTP server
.agents/skills/    Portable agent skills and slash commands
apps/              Desktop, mobile, web, and voice client applications
docs/              Architecture specs and decision documentation
```

---

<div align="center">
<sub>Built for autonomous multi-agent orchestration.</sub>
</div>
