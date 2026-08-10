#!/usr/bin/env python3
"""Render the wireframes in REPORT.md at exactly 100x30 and fail loudly if any row is not.

Run it after editing a wireframe, then paste wf/<name>.txt back into REPORT.md.
"""
import os, sys

W = 100
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "wf")
warnings = []


def cell(text, w, name="?"):
    text = text.rstrip()
    if len(text) > w:
        warnings.append(f"{name}: overflow by {len(text)-w}: {text!r}")
    return text[:w].ljust(w)


def top(title, w=W, style="light"):
    lead = "┌─ " if style == "light" else "┏━ "
    fill = "─" if style == "light" else "━"
    end = "┐" if style == "light" else "┓"
    body = lead + title + " "
    return body + fill * (w - len(body) - 1) + end


def bot(footer="", w=W):
    if footer:
        body = "└─ " + footer + " "
        return body + "─" * (w - len(body) - 1) + "┘"
    return "└" + "─" * (w - 2) + "┘"


def one(text, name="?"):
    return "│" + cell(" " + text, W - 2, name) + "│"


def two(a, b, la=48, name="?"):
    """Two side-by-side bordered panes: left inner = la-2, right inner = W-la-2."""
    return "│" + cell(" " + a, la - 2, name) + "││" + cell(" " + b, W - la - 2, name) + "│"


def two_top(ta, tb, la=48):
    left = "┌─ " + ta + " "
    left = left + "─" * (la - len(left) - 1) + "┐"
    rw = W - la
    right = "┌─ " + tb + " "
    right = right + "─" * (rw - len(right) - 1) + "┐"
    return left + right


def two_bot(fa="", fb="", la=48):
    if fa:
        left = "└─ " + fa + " "
        left = left + "─" * (la - len(left) - 1) + "┘"
    else:
        left = "└" + "─" * (la - 2) + "┘"
    rw = W - la
    if fb:
        right = "└─ " + fb + " "
        right = right + "─" * (rw - len(right) - 1) + "┘"
    else:
        right = "└" + "─" * (rw - 2) + "┘"
    return left + right


def bar(left, right="", name="?"):
    """A borderless full-width bar: left text, right text flushed to the edge."""
    pad = W - 1 - len(left) - len(right) - 1
    if pad < 1:
        warnings.append(f"{name}: bar too long: {left!r} / {right!r}")
        pad = 1
    return (" " + left + " " * pad + right + " ")[:W].ljust(W)


def emit(name, lines):
    lines = [l.ljust(W) if len(l) <= W else l for l in lines]
    bad = [(i + 1, len(l)) for i, l in enumerate(lines) if len(l) != W]
    if bad:
        warnings.append(f"{name}: wrong width rows {bad}")
    if len(lines) != 30:
        warnings.append(f"{name}: {len(lines)} rows, expected 30")
    os.makedirs(OUT, exist_ok=True)
    with open(f"{OUT}/{name}.txt", "w") as f:
        f.write("\n".join(lines) + "\n")


# --------------------------------------------------------------------------
# 1. CHAT
# --------------------------------------------------------------------------
chat = [
    top("chat · port the parser · a3f91c22 · running"),
]
body = [
    "› rebase the parser branch onto main and run the whole suite",
    "",
    "  Thinking · the branch is 6 commits behind; rebase is safer than merge",
    "  here because history has to stay linear.",
    "",
    "  I'll rebase first, then run the tests before touching anything else.",
    "⚙ Bash · git rebase origin/main",
    "  └ Successfully rebased and updated refs/heads/feat/parser.",
    "⚙ Read · core/src/parser.rs",
    "  └ 412 lines",
    "⚙ Bash · cargo test -p jod-core",
    "  └ test result: ok. 214 passed; 0 failed; 3 ignored; finished in 18.44s",
    "",
    "  Rebased cleanly — no conflicts. 214 tests pass and the three ignored ones",
    "  were already ignored on main. Pushed with --force-with-lease.",
    "",
    "✓ done · 4m12s · $0.31",
    "",
    "• ✓ audit-the-deps completed after 11m40s — Ctrl-A to open it",
    "• ◷ nightly-inbox ran at 02:00 — 3 items triaged, 1 needs you (Ctrl-K a)",
    "• ⚑ gh:ci-failed fired 2m ago → triage-ci is running",
    "• ◆ 3 memories written · 1 contradiction raised (Ctrl-K m)",
    "",
]
chat += [one(l, "chat") for l in body]
chat += [bot()]
chat += [
    top("you · 1 queued"),
    one("draft the release notes for 0.4 and post them to the team board▏", "chat"),
    bot(),
    bar("Ctrl-B delegate · Ctrl-A fleet · Ctrl-K menu · / commands · ? keys",
        "Ctrl-X stop · Ctrl-C quit", "chat"),
    bar("Claude Code · claude-opus-5 · $0.42 · ⠹ 4m12s · 2 running · 1 queued",
        "⚑ 3 unread", "chat"),
]
emit("chat", chat)

# --------------------------------------------------------------------------
# 2. FLEET
# --------------------------------------------------------------------------
left = [
    "▸ ● a3f91c22 running   4m12s cc  port-the-par",
    "  ● 77b02e10 running  11m40s agy audit-the-de",
    "  ● 1d9f0034 running  26m03s cc  triage-ci ⚑",
    "  ✓ 5c18aa93 done      2h05m cc  write-the-do",
    "  ✓ 3b7e6612 done      2h44m oc  bump-version",
    "  ✗ 0e4471bd failed    3h11m oc  migrate-stor",
    "  ✓ c0ffee11 done      5h20m cc  spec-review",
    "  ■ 91ac7752 killed   1d04h  cc  refactor-run",
    "  ✓ 8ab31d09 done     1d06h  cc  nightly-inbo",
    "  ✓ 44de1c7a done     2d01h  agy shepherd-prs",
    "  ✓ 2f9c88b1 done     2d09h  cc  write-the-sp",
    "  ✗ 6e1a0d55 failed   3d14h  oc  port-the-api",
    "  ✓ b7c2e340 done     4d02h  cc  deps-audit",
    "  ✓ 19f4aa88 done     5d11h  cc  weekly-revie",
    "",
    "─────────────────────────────────────────────",
    "14 runs · 3 running · 2 failed · $4.18 today",
    "",
    "/port         ▸ filter (2 of 14 match)",
]
right = [
    "port-the-parser",
    "a3f91c22-8e40-4b19-9a71-2c6df0e18aa3",
    "",
    "harness  Claude Code · claude-opus-5",
    "cwd      ~/repo/Jod",
    "started  16:40:12 (4m12s ago)   spend  $0.31",
    "session  sess-7f3a91c2 · pid 40118 / pgid 40118",
    "source   you, 16:40 (chat)",
    "",
    "last     Rebased cleanly — no conflicts. 214",
    "         tests pass and the three ignored ones",
    "         were already ignored on main.",
    "",
    "tools    ⚙ Bash  git rebase origin/main    ok",
    "         ⚙ Read  core/src/parser.rs        ok",
    "         ⚙ Bash  cargo test -p jod-core    ok",
    "         ⚙ Edit  core/src/parser.rs        ok",
    "         ⚙ Bash  git push --force-w-lease  ok",
    "",
    "memory   wrote 2 · read 7",
]
fleet = [two_top("fleet", "run · port-the-parser")]
for i in range(26):
    a = left[i] if i < len(left) else ""
    b = right[i] if i < len(right) else ""
    fleet.append(two(a, b, name="fleet"))
fleet += [
    two_bot("↑↓ pick · / filter", "⏎ watch · s stop · r resume · w attach"),
    bar("⏎ watch · s stop · r resume · d delegate again · w attach · / filter",
        "Esc back · ? keys", "fleet"),
    bar("fleet · 3 running · 2 failed · $4.18 today", "⚑ 3 unread", "fleet"),
]
emit("fleet", fleet)

# --------------------------------------------------------------------------
# 3. MEMORY BROWSER
# --------------------------------------------------------------------------
mleft = [
    " type    name                  conf  deg  age",
    " ────────────────────────────────────────────",
    "▸◆ blf   prefers-spec-first    0.86   17  3d",
    " ◆ blf   linear-is-truth       0.94    9  3d",
    " ◆ blf   reversible-by-default 0.91    6  1w",
    " ○ blf   ship-fast-iterate     0.31    2  6w!",
    " ● ent   reljod                1.00   41  1w",
    " ● ent   jod-cloud (vps)       1.00   12  1w",
    " ● ent   Reljod/Jod (repo)     1.00   28  1w",
    " ▤ epi   2026-08-04 spec-retro 1.00    5  6d",
    " ▤ epi   2026-07-29 vps-outage 1.00    8  12d",
    " ▦ pro   how-to-open-a-pr      1.00   11  3w",
    " ▦ pro   how-to-merge-unread   1.00    7  3w",
    " ◇ fact  tz = Asia/Manila      1.00    3  8w",
    " ◇ fact  budget cap $40/day    1.00    4  2w",
    "",
    " ────────────────────────────────────────────",
    " 142 memories · 61 beliefs · 38 entities",
    " 1 contradiction unresolved (! marks it)",
    "",
    " /spec       ▸ filter · 4 of 142 match",
]
mright = [
    "prefers-spec-first                     belief",
    "conf 0.86 · 17 edges · seen 23× · 3d ago",
    "────────────────────────────────────────────────",
    "Non-trivial work starts with a spec, not a plan.",
    "Interview until nothing material is guessed,",
    "write SPEC.md, execute it in a fresh session.",
    "",
    "▲ linked from (3)",
    "  supports     ◆ linear-is-truth",
    "  supports     ● reljod",
    "  refines      ▦ how-to-open-a-pr",
    "",
    "▼ links to (2)",
    "  contradicts  ○ ship-fast-iterate          ⚠",
    "  derived-from ▤ 2026-08-04 spec-retro",
    "",
    "provenance",
    "  first  run 2f9c88b1 write-the-spec  06-11",
    "  last   run c0ffee11 spec-review     08-07",
    "  source AGENTS.md §How work runs",
]
mem = [two_top("memory · list", "prefers-spec-first")]
for i in range(26):
    a = mleft[i] if i < len(mleft) else ""
    b = mright[i] if i < len(mright) else ""
    mem.append(two(a, b, name="memory"))
mem += [
    two_bot("↑↓ pick · / filter · t type", "g local graph · e edit · x forget"),
    bar("g graph · e edit · n new · l link · x forget · / filter · t type · s sort",
        "Esc back · ? keys", "memory"),
    bar("memory · 142 nodes · 318 edges · 1 contradiction", "⚑ 3 unread", "memory"),
]
emit("memory", mem)

# --------------------------------------------------------------------------
# 4. MEMORY — LOCAL GRAPH (focus + neighbours)
# --------------------------------------------------------------------------
g = [
    "",
    "                                  ▲  linked from — 3",
    "",
    "        ◆ linear-is-truth ──────────── supports ─────────────┐",
    "        ● reljod ───────────────────── supports ─────────────┤",
    "        ▦ how-to-open-a-pr ─────────── refines ──────────────┤",
    "                                                             │",
    "                                                             ▼",
    "                        ┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓",
    "                        ┃  ◆  prefers-spec-first                   ┃",
    "                        ┃     belief · conf 0.86 · seen 23×        ┃",
    "                        ┃     \"Non-trivial work starts with a      ┃",
    "                        ┃      spec, not a plan.\"                  ┃",
    "                        ┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛",
    "                                                             │",
    "                                                             ▼",
    "        ○ ship-fast-iterate ◀──────── contradicts ⚠ ─────────┤",
    "        ▤ 2026-08-04 spec-retro ◀──── derived-from ──────────┘",
    "",
    "                                  ▼  links to — 2",
    "",
    "  ───────────────────────────────────────────────────────────────────────────────────────────",
    "  hop 1 shows 5 of 17 edges.  ⇧+↑↓ walks the ranked edge list · ⏎ re-centres on it",
    "  ⟨  reljod  ⟩  ⟨ linear-is-truth ⟩  ⟨ prefers-spec-first ⟩         ← where you have been",
]
graph = [top("memory · local graph · prefers-spec-first")]
for i in range(26):
    graph.append(one(g[i] if i < len(g) else "", "graph"))
graph += [
    bot(" ↑↓ neighbour · ⏎ re-centre · Backspace back · h hops 1|2 · l list "),
    bar("⏎ re-centre · ↑↓ neighbour · Backspace back · h hops · f edge kind · g list",
        "Esc back · ? keys", "graph"),
    bar("memory · 142 nodes · 318 edges · focus prefers-spec-first (17 edges)",
        "⚑ 3 unread", "graph"),
]
emit("memory-graph", graph)

# --------------------------------------------------------------------------
# 5. SCHEDULES
# --------------------------------------------------------------------------
sched_rows = [
    "   name             when                next            in       last            ago    7d      ",
    "   ───────────────────────────────────────────────────────────────────────────────────────────  ",
    " ▸ ● nightly-inbox   02:00 every day     Aug 11 02:00    9h14m    Aug 10 02:00    14h    ▇▇▇▇▇▇▇ ",
    "   ● pr-shepherd     every 30 minutes    Aug 10 17:00      14m    Aug 10 16:30    16m    ▇▇▇▅▇▇▇ ",
    "   ● weekly-review   Mon 08:00           Aug 17 08:00    6d15h    Aug 10 08:00     8h    ▇▁▇▇▇▇▇ ",
    "   ● finance-sync    09:00 Mon–Fri       Aug 11 09:00   16h14m    Aug 08 09:00     2d    ▇▇▇✗▇▇▇ ",
    "   ● vps-healthcheck every 15 minutes    Aug 10 16:59       13s   Aug 10 16:45     1m    ▇▇▇▇▇▇▇ ",
    "   ‖ deps-audit      Sun 03:00           —  paused        —       Aug 03 03:00     7d    ▇▇▁▁▁▁▁ ",
    "   ✗ notion-sync     04:00 every day     Aug 11 04:00   11h14m    Aug 10 04:00    12h    ✗✗▇▇▇✗✗ ",
    "   ● backup-jod-db   23:30 every day     Aug 10 23:30    6h44m    Aug 09 23:30    17h    ▇▇▇▇▇▇▇ ",
    "                                                                                                 ",
    "   ────────────────────────────────────────────────────────────────────────────────────────────  ",
    "   nightly-inbox                                          cron  0 2 * * *   ·  Asia/Manila       ",
    "                                                                                                 ",
    "   prompt   Triage the Linear inbox. Close what is done, escalate what is blocked, and leave a   ",
    "            one-line note on anything you touched.                                                ",
    "   runs as  Claude Code · claude-opus-5 · ~/repo/Jod · permission: acceptEdits                    ",
    "   policy   overlap: skip  ·  missed run: run once on wake  ·  timeout 20m  ·  budget $2/run      ",
    "                                                                                                 ",
    "   history  Aug 10 02:00  ✓  4m18s  $0.44   3 items triaged, 1 escalated                          ",
    "            Aug 09 02:00  ✓  3m51s  $0.39   1 item triaged                                        ",
    "            Aug 08 02:00  ✓  5m02s  $0.51   6 items triaged, 2 escalated                          ",
    "            Aug 07 02:00  ✓  4m44s  $0.47   2 items triaged                                       ",
    "            Aug 06 02:00  ✓  2m19s  $0.22   nothing to do                                         ",
]
sc = [top("schedules · 8 · next: vps-healthcheck in 13s")]
for i in range(26):
    sc.append(one(sched_rows[i] if i < len(sched_rows) else "", "sched"))
sc += [
    bot(" ↑↓ pick · ⏎ open the last run · r run now · p pause · e edit · n new · x delete "),
    bar("⏎ last run · r run now · p pause/resume · e edit · n new · x delete · / filter",
        "Esc back · ? keys", "sched"),
    bar("schedules · 6 armed · 1 paused · 1 failing · next in 13s", "⚑ 3 unread", "sched"),
]
emit("schedules", sc)

# --------------------------------------------------------------------------
# 6. GOALS
# --------------------------------------------------------------------------
goal_rows = [
    "   name                 cadence      progress            last      next     state              ",
    "   ──────────────────────────────────────────────────────────────────────────────────────────  ",
    " ▸ ◎ inbox-to-zero       hourly       ▓▓▓▓▓▓▓░░░  71%     42m ago   in 18m   running  iter 118  ",
    "   ◎ keep-ci-green       continuous   ▓▓▓▓▓▓▓▓▓▓ 100%      4m ago   in  6m   satisfied iter 903 ",
    "   ◎ ship-ios-client     daily        ▓▓▓░░░░░░░  31%      6h ago   in 18h   waiting  iter  24  ",
    "   ◎ reduce-vps-spend    weekly       ▓▓░░░░░░░░  18%      3d ago   in  4d   blocked  iter   6  ",
    "   ◎ zero-open-prs       every 30m    ▓▓▓▓▓▓▓▓░░  84%     11m ago   in 19m   running  iter 412  ",
    "                                                                                                ",
    "   ─────────────────────────────────────────────────────────────────────────────────────────── ",
    "   inbox-to-zero                                                          started 2026-06-02    ",
    "                                                                                                ",
    "   objective  Keep the Linear inbox at zero open items older than 48 hours.                     ",
    "   done when  ☑ no item older than 48h    ☑ every open item has an owner                        ",
    "              ☐ no item blocked without a written reason   ← 3 items fail this                  ",
    "   stop if    budget $25/week spent  ·  6 iterations with no progress  ·  you say stop          ",
    "   budget     $11.40 of $25.00 this week   ▓▓▓▓▓░░░░░                                           ",
    "                                                                                                ",
    "   iterations 118  16:02  +4 items closed, 3 still blocked            5m11s  $0.38  ✓           ",
    "              117  15:02  +1 item closed                              2m40s  $0.19  ✓           ",
    "              116  14:02  nothing to do                               0m48s  $0.04  ✓           ",
    "              115  13:02  +2 closed, escalated ENG-441 to you         6m22s  $0.51  ✓           ",
    "              114  12:02  harness timed out after 20m                20m00s  $1.02  ✗           ",
    "                                                                                                ",
    "   escalations  ENG-441 needs a decision from you — raised 13:02, still open                    ",
]
go = [top("goals · 5 · 2 running · 1 blocked · 1 escalation waiting on you")]
for i in range(26):
    go.append(one(goal_rows[i] if i < len(goal_rows) else "", "goals"))
go += [
    bot(" ↑↓ pick · ⏎ open the last iteration · r run now · p pause · e edit · n new "),
    bar("⏎ last iteration · r run now · p pause · e edit · n new · a answer escalation",
        "Esc back · ? keys", "goals"),
    bar("goals · 2 running · 1 blocked · $11.40 this week", "⚑ 3 unread", "goals"),
]
emit("goals", go)

# --------------------------------------------------------------------------
# 7. WEBHOOKS
# --------------------------------------------------------------------------
hook_rows = [
    "   name             repo                event                        runs        24h  last     ",
    "   ──────────────────────────────────────────────────────────────────────────────────────────  ",
    " ▸ ● pr-opened      Reljod/Jod          pull_request.opened          review-pr    18   2m  ✓    ",
    "   ● ci-failed      Reljod/Jod          workflow_run.completed ✗     triage-ci     3  41m  ✓    ",
    "   ● issue-labeled  Reljod/Jod          issues.labeled [jod]         plan-issue    6   4h  ✓    ",
    "   ● review-asked   Reljod/Jod          pull_request.review_req      review-pr    11   1h  ✓    ",
    "   ○ push-main      Reljod/jod-cloud    push refs/heads/main         deploy-vps    0   —   —    ",
    "   ✗ release-cut    Reljod/Jod          release.published            announce      1   2d  ✗    ",
    "                                                                                                ",
    "   ──────────────────────────────────────────────────────────────────────────────────────────  ",
    "   pr-opened                                              created 2026-07-14 · 214 deliveries   ",
    "                                                                                                ",
    "   endpoint   https://jod.reljod.dev/hooks/gh/pr-opened          secret  ✓ verified 2m ago      ",
    "   match      event = pull_request  ·  action = opened  ·  base = main  ·  draft = false        ",
    "   runs       review-pr   Claude Code · ~/repo/Jod · permission: plan · budget $1.50            ",
    "   prompt     Review PR #{{number}} \"{{title}}\" by {{author}} against REVIEW.md. Veto only.     ",
    "   policy     dedupe by delivery id · 1 run per PR at a time · queue depth 4 · retry 3×         ",
    "                                                                                                ",
    "   deliveries 16:42  8f2a1c  PR #212 port the parser        ✓ 202  → a3f91c22  running          ",
    "              15:10  71b93e  PR #211 bump versions          ✓ 202  → 3b7e6612  ✓ clear          ",
    "              11:58  2c0dd4  PR #210 migrate the store      ✓ 202  → 0e4471bd  ✗ failed         ",
    "              09:31  a41f77  PR #209 write the docs         ✓ 202  → 5c18aa93  ✓ vetoed         ",
    "              08:02  55e0b1  PR #208 spec review            ✓ 202  → c0ffee11  ✓ clear          ",
]
wh = [top("webhooks · 6 · 5 armed · 1 failing · 214 deliveries all time")]
for i in range(26):
    wh.append(one(hook_rows[i] if i < len(hook_rows) else "", "hooks"))
wh += [
    bot(" ↑↓ pick · ⏎ open the delivery's run · t test with a sample payload · p pause · e edit "),
    bar("⏎ open run · t test payload · p pause · e edit · n new · c copy URL · x delete",
        "Esc back · ? keys", "hooks"),
    bar("webhooks · 5 armed · 1 failing · 28 deliveries today", "⚑ 3 unread", "hooks"),
]
emit("webhooks", wh)

# --------------------------------------------------------------------------
# 8. ACTIVITY
# --------------------------------------------------------------------------
act_rows = [
    "   today — Monday 10 August                                                                     ",
    "   ──────────────────────────────────────────────────────────────────────────────────────────   ",
    " ▸ ● 16:44  run      ✓ port-the-parser finished · 4m12s · $0.31 · 214 tests pass                 ",
    "   ● 16:42  hook     ⚑ pr-opened fired (PR #212) → triage started as a3f91c22                    ",
    "   ● 16:32  hook     ⚑ ci-failed fired → triage-ci running 26m                                   ",
    "     16:30  cron     ◷ pr-shepherd ran · 3 PRs swept · 1 merged · 0 vetoed · 1m04s              ",
    "   ● 16:02  goal     ◎ inbox-to-zero iteration 118 · 71% (+4) · needs you on ENG-441             ",
    "     15:41  memory   ◆ 3 memories written by audit-the-deps · 1 contradiction raised             ",
    "     15:10  hook     ⚑ pr-opened (PR #211) → 3b7e6612 · clear                                    ",
    "     14:55  run      ✗ migrate-store failed · 3h11m · exit 1 · \"store is locked\"                 ",
    "     14:02  goal     ◎ inbox-to-zero iteration 116 · nothing to do                               ",
    "     12:02  goal     ◎ inbox-to-zero iteration 114 · ✗ harness timed out after 20m               ",
    "     09:00  cron     ◷ finance-sync skipped — previous run still going (overlap: skip)          ",
    "                                                                                                 ",
    "   yesterday — Sunday 9 August                                                                   ",
    "   ──────────────────────────────────────────────────────────────────────────────────────────   ",
    "     23:30  cron     ◷ backup-jod-db ✓ 41s · 18.2 MB                                            ",
    "     04:00  cron     ◷ notion-sync ✗ 401 from Notion — token expired  (3rd failure in a row)    ",
    "     02:00  cron     ◷ nightly-inbox ✓ 3m51s · 1 item triaged                                   ",
    "                                                                                                 ",
    "   ──────────────────────────────────────────────────────────────────────────────────────────   ",
    "   filter  [all]  runs  cron  goals  hooks  memory      only unread: off      f cycles           ",
]
ac = [top("activity · 3 unread · 1 needs you")]
for i in range(26):
    ac.append(one(act_rows[i] if i < len(act_rows) else "", "activity"))
ac += [
    bot(" ↑↓ pick · ⏎ jump to what it is about · m mark read · M mark all read · f filter "),
    bar("⏎ jump to it · m mark read · M mark all · u unread only · f filter source",
        "Esc back · ? keys", "activity"),
    bar("activity · 3 unread · last event 2m ago", "⚑ 3 unread", "activity"),
]
emit("activity", ac)

# --------------------------------------------------------------------------
# 9. WHICH-KEY OVERLAY (over chat)
# --------------------------------------------------------------------------
wk_under = [
    "  Rebased cleanly — no conflicts. 214 tests pass and the three ignored ones",
    "  were already ignored on main. Pushed with --force-with-lease.",
    "",
    "✓ done · 4m12s · $0.31",
    "",
]
wk = [top("chat · port the parser · a3f91c22 · running")]
rows = []
for l in wk_under:
    rows.append(one(l, "wk"))
overlay = [
    '       ┌─ Ctrl-K ───────────────────────────────────────────────────┐',
    '       │  c  chat            the conversation                       │',
    '       │  f  fleet           14 runs · 3 running · 2 failed         │',
    '       │  m  memory          142 nodes · 1 contradiction            │',
    '       │  s  schedules       8 · next vps-healthcheck in 13s        │',
    '       │  g  goals           5 · 1 blocked · 1 needs you            │',
    '       │  h  hooks           6 webhooks · 1 failing                 │',
    '       │  a  activity        3 unread                               │',
    '       │  t  team            crew · 4 members · 6 open tasks        │',
    '       │                                                            │',
    '       │  n  new…            n s schedule · n g goal · n h hook     │',
    '       │  ?  keys            the whole keymap                       │',
    '       └─ Esc cancels · any other key is ignored ───────────────────┘',
]
for l in overlay:
    rows.append(one(l, "wk"))
while len(rows) < 26:
    rows.append(one("", "wk"))
wk += rows[:26]
wk += [
    bot(),
    bar("Ctrl-K … waiting for a key", "Esc cancels", "wk"),
    bar("Claude Code · claude-opus-5 · $0.42 · ⠹ 4m12s · 2 running", "⚑ 3 unread", "wk"),
]
emit("which-key", wk)


# --------------------------------------------------------------------------
# 10. TASKS (the board, promoted out of the team panel)
# --------------------------------------------------------------------------
task_rows = [
    "   id                title                                owner    state    run       age  ",
    "   ─────────────────────────────────────────────────────────────────────────────────────   ",
    " ▸ ● port-the-parser Port the parser to the new AST       a3f91c22 running  a3f91c22  4m   ",
    "   ● triage-ci       Work out why CI went red on main     1d9f0034 running  1d9f0034  26m  ",
    "   ◐ write-the-docs  Write the docs for the events API    scout    claimed  5c18aa93  2d   ",
    "   ◐ bump-versions   Bump every crate to 0.4              lead     claimed  3b7e6612  2h   ",
    "   ○ migrate-store   Move the run transport into SQLite   —        open     —         3d   ",
    "   ○ ios-transcript  Match the TUI transcript on iOS      —        open     —         5d   ",
    "   ⚠ deploy-vps      Redeploy jod-cloud after the bump    —        blocked  —         1d   ",
    "   ✓ spec-review     Review SPEC.md before implementing   reljod   done     c0ffee11  5h   ",
    "   ✓ write-the-spec  Write SPEC.md for the transport      reljod   done     2f9c88b1  2d   ",
    "                                                                                            ",
    "   ─────────────────────────────────────────────────────────────────────────────────────   ",
    "   migrate-store                                        open · unclaimed · on board 3d      ",
    "                                                                                            ",
    "   what      Move the run transport out of tmux + JSONL and into SQLite, so a run is a       ",
    "             detached process group supervised by jod-run writing straight to the store.     ",
    "   check     cargo test -p jod-core -p jod-supervisor  ·  no test may be skipped             ",
    "   blocked   nothing                                                                         ",
    "   blocks    deploy-vps  (⚠ waiting on this)                                                 ",
    "   spec      SPEC.md  ·  920 lines  ·  last touched 2d ago                                   ",
    "                                                                                             ",
    "   history   3d ago  put on the board by reljod                                              ",
    "             2d ago  claimed by lead, released 4h later — \"needs the spec first\"            ",
    "                                                                                             ",
    "   d delegates this to an agent: a fresh run seeded with the title, the check and the spec.   ",
]
tk = [top("tasks · 12 open · 3 claimed · 1 blocked · 4 done this week")]
for i in range(26):
    tk.append(one(task_rows[i] if i < len(task_rows) else "", "tasks"))
tk += [
    bot(" ↑↓ pick · ⏎ mark done · d delegate to an agent · c claim · o open its run · n new "),
    bar("⏎ mark done · d delegate · c claim · o open run · n new · x remove · / filter",
        "Esc back · ? keys", "tasks"),
    bar("tasks · 12 open · 1 blocked · 2 being worked right now", "⚑ 3 unread", "tasks"),
]
emit("tasks", tk)

if warnings:
    print("WARNINGS:")
    for w in warnings:
        print(" -", w)
    sys.exit(1)
print("ok — all wireframes are exactly 100x30")
